# Lab 1：多道程序与分时多任务

## 一、实现功能

本实验实现了系统调用 sys_trace，支持读取一个字节、写入一个字节，以及查询当前任务的系统调用次数。每个任务独立保存计数。内核在分发系统调用前先计数，因此查询 sys_trace 的次数时，本次调用也会被算进去。

## 二、简答题

### 1. U 态异常测试

本次使用课程 Docker 环境和仓库自带的 RustSBI Prototyper 0.4.1，
以 `CHAPTER=3 TEST=2 BASE=1` 运行第二章的异常用例。

| 用例 | 实际观察 |
| --- | --- |
| `ch2b_bad_address` | 内核日志报告 `PageFault`，非法访问地址为 `0x0`，触发异常的指令地址为 `0x804003a4`。内核终止了该应用。 |
| `ch2b_bad_instructions` | 用户态执行受限指令，内核日志报告 `IllegalInstruction`，并终止了该应用。 |
| `ch2b_bad_register` | 用户态访问受限寄存器，内核日志报告 `IllegalInstruction`，并终止了该应用。 |

三个异常应用被终止后，其他用户程序仍继续运行。`power_3`、`power_5`
和 `power_7` 均输出 `OK`，说明这些异常没有阻止内核继续调度其他任务。
### 2. trap.S 中的 7 个问题

1. 刚进入 `__restore` 时，`sp` 指向内核栈上的 `TrapContext`。它有两个使用场景：第一次启动用户程序时，恢复预先准备好的上下文；内核处理完陷阱后，恢复保存的上下文并返回用户程序。

2. 代码单独恢复 `sstatus`、`sepc` 和用户栈指针。`sstatus` 的 `SPP` 位决定 `sret` 返回到哪个特权级。`sepc` 指定返回后从哪条指令继续执行。用户栈指针先放入 `sscratch`，等最后一步再交换到 `sp`。

3. `x2` 就是 `sp`。此时还要用内核栈上的 `sp` 读取其他寄存器，所以用户栈指针要最后恢复。`x4` 是 `tp`，这版教学代码假定应用不用它，所以跳过保存和恢复。

4. `__restore` 最后的 `csrrw sp, sscratch, sp` 执行后，`sp` 指向用户栈，`sscratch` 保存内核栈指针，供下一次陷阱使用。

5. `sret` 完成返回。如果恢复后的 `sstatus.SPP` 表示 U 态，CPU 就切换到 U 态，并从 `sepc` 指定的地址继续执行。

6. `__alltraps` 开头的 `csrrw sp, sscratch, sp` 执行后，`sp` 指向内核栈，`sscratch` 暂存用户栈指针。随后代码在内核栈上为 `TrapContext` 分配空间。

7. 在系统调用场景中，用户程序执行 `ecall` 触发异常。CPU 将这个异常交给 S 态处理，进入 `__alltraps`。此时第一条 `csrrw` 负责交换栈指针。

## 三、本地测试结果

使用课程 Docker 环境运行 `make -C os run CHAPTER=3 TEST=3 BASE=2`，日志出现 `Test trace OK!`，基础用例也显示通过。

## 四、荣誉准则

1. 实验协助：我使用 ChatGPT 获取了 `sys_trace` 的修改脚本，其中包括按任务保存系统调用计数、在分发前计数，以及实现字节读写和次数查询。ChatGPT 还协助排查了 Docker 构建和内核链接问题，并提供了 `trap.S` 七道简答题的解释和报告整理建议。
2. 参考资料：我参考了课程的第三章实验指导和仓库 README，用于核对 `sys_trace` 的接口要求、实验运行方式和报告要求。
3. 实验验证：我在自己的 WSL Ubuntu 和课程 Docker 环境中执行了构建和测试。报告中的 `Test trace OK!` 和三个 U 态异常用例的结果来自我本机的终端输出。
