//! index node(inode, namely file control block) layer
//!
//! The data struct and functions for the inode layer that service file-related system calls
//!
//! NOTICE: The difference between [`Inode`] and [`DiskInode`]  can be seen from their names: DiskInode in a relatively fixed location within the disk block, while Inode Is a data structure placed in memory that records file inode information.
use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};

/// Inode struct in memory
pub struct Inode {
    /// The block id of the inode
    block_id: usize,
    /// The offset of the inode in the block
    block_offset: usize,
    /// The file system
    fs: Arc<Mutex<EasyFileSystem>>,
    /// The block device
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a new Disk Inode
    ///
    /// We should not acquire efs lock here.
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// read the content of the disk inode on disk with 'f' function
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// modify the content of the disk inode on disk with 'f' function
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// find the disk inode id according to the file with 'name' by search the directory entries in the disk inode with Directory type
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if !dirent.name().is_empty() && dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// find the disk inode of the file with 'name'
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// increase the size of file( also known as 'disk inode')
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// create a file with 'name' in the root directory
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &mut DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if !Self::valid_name(name) || self.modify_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.insert_dir_entry(name, new_inode_id, &mut fs);

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    // ChatGPT assisted with directory entry handling and hard-link accounting.
    fn valid_name(name: &str) -> bool {
        !name.is_empty() && name.len() <= DIRENT_SZ - 5
            && !name.as_bytes().contains(&0) && !name.contains('/')
    }

    fn insert_dir_entry(&self, name: &str, inode_id: u32, fs: &mut MutexGuard<EasyFileSystem>) {
        self.modify_disk_inode(|root| {
            let size = root.size as usize;
            let mut entry = DirEntry::empty();
            let offset = (0..size).step_by(DIRENT_SZ).find(|&offset| {
                root.read_at(offset, entry.as_bytes_mut(), &self.block_device);
                entry.name().is_empty()
            }).unwrap_or(size);

            if offset == size {
                self.increase_size((size + DIRENT_SZ) as u32, root, fs);
            }
            let entry = DirEntry::new(name, inode_id);
            assert_eq!(root.write_at(offset, entry.as_bytes(), &self.block_device), DIRENT_SZ);
        });
    }

    /// Create another directory entry for an existing inode.
    pub fn link(&self, old: &str, new: &str) -> bool {
        if !Self::valid_name(old) || !Self::valid_name(new) {
            return false;
        }
        let mut fs = self.fs.lock();
        let inode_id = self.read_disk_inode(|root| {
            if self.find_inode_id(new, root).is_some() {
                return None;
            }
            self.find_inode_id(old, root)
        });
        let Some(inode_id) = inode_id else {
            return false;
        };
        self.insert_dir_entry(new, inode_id, &mut fs);
        block_cache_sync_all();
        true
    }

    /// Remove one name from the directory while preserving the inode.
    pub fn unlink(&self, name: &str) -> bool {
        if !Self::valid_name(name) {
            return false;
        }
        let _fs = self.fs.lock();
        let removed = self.modify_disk_inode(|root| {
            let mut entry = DirEntry::empty();
            for offset in (0..root.size as usize).step_by(DIRENT_SZ) {
                root.read_at(offset, entry.as_bytes_mut(), &self.block_device);
                if entry.name() == name {
                    let empty = DirEntry::empty();
                    assert_eq!(root.write_at(offset, empty.as_bytes(), &self.block_device), DIRENT_SZ);
                    return true;
                }
            }
            false
        });
        if removed {
            block_cache_sync_all();
        }
        removed
    }

    /// Return the inode number and the number of names pointing to it in this directory.
    pub fn link_stat(&self, inode: &Inode) -> (u64, u32) {
        let fs = self.fs.lock();
        let (first_block, first_offset) = fs.get_disk_inode_pos(0);
        let position = (inode.block_id - first_block as usize) * super::BLOCK_SZ
            + inode.block_offset - first_offset;
        let inode_id = position / core::mem::size_of::<DiskInode>();

        let nlink = self.read_disk_inode(|root| {
            let mut entry = DirEntry::empty();
            (0..root.size as usize).step_by(DIRENT_SZ).filter(|&offset| {
                root.read_at(offset, entry.as_bytes_mut(), &self.block_device);
                !entry.name().is_empty() && entry.inode_id() as usize == inode_id
            }).count() as u32
        });
        (inode_id as u64, nlink)
    }

    /// create a directory with 'name' in the root directory
    ///
    /// list the file names in the root directory
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if !dirent.name().is_empty() {
                    v.push(String::from(dirent.name()));
                }
            }
            v
        })
    }
    /// Read the content in offset position of the file into 'buf'
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write the content in 'buf' into offset position of the file
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Set the file(disk inode) length to zero, delloc all data blocks of the file.
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
}
