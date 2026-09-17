# ToDay

一个用 Rust 写的跨平台极简日记桌面应用：每天一条纯文本记录，数据存在本机的一个 SQLite 文件里，正文逐字段加密。

## 运行

```bash
cargo run
```

首次启动会让你设一个主密码（至少 8 个字符）。之后每次启动输入密码解锁。
主密码不会保存、也没有找回入口——忘了就打不开这本日记。

## 数据

- 位置：系统约定的应用数据目录，文件名固定为 `today.db`
  - Windows：`%APPDATA%\ToDay\data\today.db`
  - macOS：`~/Library/Application Support/ToDay/today.db`
  - Linux：`~/.local/share/today/today.db`（`directories` 在 Linux 上会把名字转小写）
- 表结构：`entries(date, body, updated_at, format)`
  - `date` 是明文（`YYYY-MM-DD`）：日历打点和按月查询需要它
  - `body` 是密文（`nonce || ChaCha20-Poly1305 密文`），密钥由 Argon2id 从主密码派生
- 所以直接打开 `today.db` 能看出"哪几天写过"，但看不到任何一个字的内容

细节见 [`docs/architecture.md`](docs/architecture.md)。

## 功能

- 按日期快速切换，每天一条纯文本记录
- 左侧当月日历：点中的那天是墨块，今天是一圈细线，写过东西的日子底下点一个小圆点
- 右侧一日一记：行首是日号与星期，正文没有边框，只有行与行之间那根细线
- 顶栏放品牌、当前日期、"回到今天"和锁定按钮；底栏放本月统计与快捷键提示
- 快捷键：`Enter` 跳到下一行，右键删除某一天；删除后底部浮出一条 8 秒的撤销提示
- 写入时机：打字停 0.4 秒落盘；离开一行、删除、撤销、切换日期/月份、锁定、关窗都立刻落盘

## 安全

- Argon2id（m=19 MiB, t=2, p=1）+ 16 字节随机盐派生密钥，参数随库存放，将来调参不用改代码
- ChaCha20-Poly1305 逐字段加密，每条密文新随机 nonce
- 日期作为 AAD 一起认证：把某天的密文搬到另一天会解不开
- 密钥只活在解锁后的会话里，锁定即销毁；密码输入缓冲用完立刻清零
- 没有导出、没有云同步，数据只在本机

## 还没做

导出纯文本（`.txt`）、修改主密码（重新加密）、闲置自动锁定、多设备同步、全文搜索。

## 开发

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test          # 包含加密、存储、落盘策略与端到端（无需显示器）
```

工程准则见 [`Agents.md`](Agents.md)，模块图与决策记录见 [`docs/architecture.md`](docs/architecture.md)。
