<img src="assets/logo.png" width="96" alt="srmgui logo">

# srmgui

一个基于 [GPUI Kit](https://crates.io/crates/gpui-kit) 的桌面小工具，用于查看、测速并切换 Rust Cargo 的镜像源，可按所在地区推荐并一键应用，同时支持企业私有 Registry 的索引与认证 token 管理、Cargo 缓存清理，配置直接写入 Cargo 配置与凭据文件。

## 界面预览

| 内置源（测速结果） | 自定义源 |
| --- | --- |
| ![内置源](.screenshots/01-builtin-sources.png) | ![自定义源](.screenshots/04-custom-sources.png) |
| 私有 Registry | 添加私有 Registry |
| ![私有 Registry](.screenshots/05-private-registry.png) | ![添加私有 Registry](.screenshots/06-private-add.png) |
| 配置健康检查 | 写入前预览 |
| ![配置健康检查](.screenshots/07-health-check.png) | ![写入前预览](.screenshots/08-write-preview.png) |
| 配置快照 | 快照对比 |
| ![配置快照](.screenshots/10-config-snapshots.png) | ![快照对比](.screenshots/11-snapshot-diff.png) |
| Cargo 缓存管理 | 日志面板 |
| ![Cargo 缓存管理](.screenshots/12-cargo-cache.png) | ![日志面板](.screenshots/03-log.png) |
| 深色模式 | 应用设置 |
| ![深色模式](.screenshots/02-dark-mode.png) | ![应用设置](.screenshots/09-settings.png) |

## 功能特性

- **内置镜像源**：内置 RsProxy、阿里云、华为云、中科大 USTC、清华 TUNA、上海交大 SJTUG 等 14 个国内镜像与官方 crates.io，一键切换。
- **在线清单订阅**：启动时从仓库的 `sources.json` 拉取最新内置源清单，新增镜像无需发版；结果缓存在本地，离线仍可用，也可点击「刷新清单」手动更新。
- **地区识别与推荐**：识别出口 IP 所在的国家与省份，结合实测下载速度与索引延迟给出推荐条；中国大陆优先同省可用源，境外推荐官方源，识别失败时沿用上次结果。
- **四层自动探测**：逐层检测并展示每个源是否真正可用：
  1. TCP 连接
  2. HTTP 索引探测
  3. 索引文件解析（按 sparse registry 协议校验 `config.json`）
  4. 下载测速（报告速度）
- **定时刷新**：启动后自动测速，并按设置的周期（默认 180 秒）重新探测一轮。
- **自定义源**：支持添加任意 sparse registry，按标识名判重、自动补全 `sparse+https://` 前缀。
- **私有 Registry**：独立管理 `[registries.<name>]` 索引与认证 token，支持 sparse / git 索引，适配企业私有镜像；token 单独写入 `~/.cargo/credentials.toml`，不会进入项目配置。
- **导入 / 导出**：自定义源可导出为 srmgui 自定义的 `.srmgui` 文件，也可从该格式导入并自动合并（跳过内置源与重复标识）。
- **主题切换**：跟随系统 / 浅色 / 深色。
- **细节体验**：固定列宽布局，过长文字循环滚动显示，悬停显示完整详情 tips。
- **源分享**：任一源可生成 `srmgui://source?data=...` 分享链接，复制给他人后可用「导入链接」直接导入。
- **Cargo 缓存管理**：按来源统计 registry 索引、下载包与解压源码的占用，可删除单个来源缓存并清理无效条目。
- **日志面板**：按级别多选过滤、关键字搜索，并支持一键导出诊断包（配置、设置、探测结果与运行环境）。
- **多语言界面**：内置简体中文、English、日本語、한국어、Deutsch、Français、Español、Русский 8 种语言，工具栏切换后立即生效并持久化。
- **写入前预览**：点击「应用」先展示合并后的配置差异（行级 diff，`+` / `-` 着色），确认后才落盘，取消不产生任何改动。
- **配置快照与回滚**：每次写入前自动保留配置快照（默认最多最近 10 份），可一键恢复到任意一次；快照标签标注「切换前源」，与内容始终一致。
- **快照对比**：每条快照支持「与当前对比」，也可选定另一条快照互相对比，直观看出切换前后的配置变化。
- **配置健康检查**：扫描 `config.toml` 中被其他工具改坏后的常见残留——重复表头、指向不存在源的 `replace-with`、孤立 `[source.x]` 段与 TOML 语法错误，并支持一键修复。
- **自动检查更新**：启动后台检查 GitHub Releases 并按设置的周期（默认 6 小时）复查，发现新版本时右上角提示；点击可选择用系统下载工具或应用内下载。
- **安全写入**：写入前自动备份原配置为 `config.toml.bak`。
- **应用设置**：工具栏「设置」可集中调整网络超时、重定向次数、测速下载上限与示例 crate、测速 / 更新检查周期、快照保留数量以及日志参数；输入越界会给出提示，支持一键恢复默认，保存后立即按新参数生效。
- **命令行子命令**：同一个可执行文件提供 `status` / `list` / `use` / `probe` / `custom` / `registry` / `snapshot` / `scope` / `doctor` 等命令，与图形界面共享同一份配置与快照，方便脚本与 CI 调用。

## 内置源

| 标识名 | 名称 | 地区 | Registry URL | 说明 |
| --- | --- | --- | --- | --- |
| `rsproxy` | RsProxy（字节） | 北京 | `sparse+https://rsproxy.cn/index/` | 国内实测最快，推荐日常使用 |
| `aliyun` | 阿里云 | 浙江 | `sparse+https://mirrors.aliyun.com/crates.io-index/` | 企业级镜像，下载稳定 |
| `ustc` | 中科大 USTC | 安徽 | `sparse+https://mirrors.ustc.edu.cn/crates.io-index/` | 华东教育网镜像，连通性好 |
| `tuna` | 清华 TUNA | 北京 | `sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/` | 华北教育网镜像，更新及时 |
| `sjtu` | 上海交大 SJTUG | 上海 | `sparse+https://mirrors.sjtug.sjtu.edu.cn/crates.io-index/` | 华东高校镜像，适合应急 |
| `huawei` | 华为云 | 广东 | `sparse+https://mirrors.huaweicloud.com/crates.io-index/` | 企业级镜像，覆盖全国 |
| `cernet` | CERNET 联合镜像 | — | `sparse+https://mirrors.cernet.edu.cn/crates.io-index/` | 教育网联合镜像，自动调度 |
| `nju` | 南京大学 | 江苏 | `sparse+https://mirrors.nju.edu.cn/crates.io-index/` | 江苏高校镜像，华东备用 |
| `bfsu` | 北外 BFSU | 北京 | `sparse+https://mirrors.bfsu.edu.cn/crates.io-index/` | 北京教育网镜像，华北备用 |
| `sdu` | 山东大学 | 山东 | `sparse+https://mirrors.sdu.edu.cn/crates.io-index/` | 山东高校镜像，华北备用 |
| `hust` | 华中科技大学 | 湖北 | `sparse+https://mirrors.hust.edu.cn/crates.io-index/` | 华中高校镜像，中部备用 |
| `zju` | 浙江大学 | 浙江 | `sparse+https://mirrors.zju.edu.cn/crates.io-index/` | 浙江高校镜像，华东备用 |
| `neusoft` | 东软 | 辽宁 | `sparse+https://mirrors.neusoft.edu.cn/crates.io-index/` | 东北镜像，北方备用 |
| `nyist` | 南阳理工学院 | 河南 | `sparse+https://mirror.nyist.edu.cn/crates.io-index/` | 河南高校镜像，中原备用 |
| `official` | 官方 crates.io | 全球 | `sparse+https://index.crates.io/` | 默认源，`cargo publish` 前建议切回 |

内置源清单来自仓库根目录的 [`sources.json`](sources.json)（格式 `srmgui.manifest` v3），应用启动时会拉取最新版本并缓存到 `<应用目录>/srmgui-sources.json`；离线或拉取失败时使用本地缓存，缓存也没有时回退到编译期内置清单。

## 使用

1. 启动程序，等待自动测速完成（每行依次显示 TCP / HTTP / 解析 / 下载四个步骤的结果）；顶部推荐条会显示识别到的地区与推荐源，可点击「应用推荐源」直接切换。
2. 点击目标源右侧的「应用」，在弹出的「写入前预览」中查看目标路径、变更摘要与 `+` / `-` 着色 diff，确认无误后点击「确认写入」，配置才会写入 `~/.cargo/config.toml`（优先使用 `$CARGO_HOME`）；点击「取消」不会产生任何改动。
3. 当前生效的源会在行尾显示「当前生效 ✅」。
4. 如需回退，点击标题栏的「快照」按钮，在列表中选择某次快照点击「恢复」即可。
5. 如果配置被其他工具改坏，点击标题栏的「健康检查」，扫描并一键修复问题。
6. 需要清理磁盘时点击标题栏的「缓存」，查看 registry 索引、下载包与源码的占用，并按来源删除。
7. 选中任一源后点击「分享」可复制 `srmgui://source?data=...` 链接；收到链接时点击「导入链接」粘贴即可。
8. 如需调整超时、测速 / 更新周期、快照保留数与日志参数，点击标题栏的「设置」修改，保存后立即生效；右上角语言菜单可切换 8 种界面语言。

### 写入前预览

「应用」不再直接落盘，而是先计算「现有配置 + 新源」的合并结果，并与当前文件做行级对比：

- 顶部显示本次写入的目标文件路径；
- 中间显示新增 / 删除行数与 diff 内容（`+` 为新增，`-` 为删除）；
- 只在点击「确认写入」后才写入，取消则完全不改动配置。

### 地区推荐

启动后（以及每轮测速结束）会按出口 IP 识别所在国家与省份，并在源列表顶部给出推荐条：

- 中国大陆：优先同省可用源，其次按实测下载速度与索引延迟排序；
- 其他地区：优先官方 crates.io 等全球源；
- 尚无测速结果时使用内置兜底优先级，识别失败则沿用上次结果；
- 点击「应用推荐源」直接进入写入前预览流程。

### 源清单与分享

- **在线清单**：启动与每次更新检查时拉取仓库根目录的 `sources.json`，成功后替换列表并写入本地缓存；也可点击「刷新清单」手动触发。
- **分享源**：选中源后点击「分享」，复制 `srmgui://source?data=...` 链接（URL-safe Base64 编码的标识名、名称、URL、说明与地区）。
- **导入链接**：粘贴他人分享的链接即可导入，已存在的标识会自动跳过并提示数量。

### Cargo 缓存管理

点击标题栏的「缓存」扫描 `$CARGO_HOME`（默认 `~/.cargo`）：

- 按来源汇总 registry 索引、下载包与解压源码的占用，兼容 sparse 与 git 索引；
- 支持删除单个来源的缓存，以及清理无法对应到任何来源的无效条目；
- 扫描结果会缓存在应用目录，下次打开直接展示，缓存体积变化后自动重新扫描。

### 配置快照与回滚

每次真正写入前，srmgui 会把当时的配置内容保存为一份快照：

- 快照保存在 `<应用目录>/snapshots/`，最多保留最近 10 份，超出后自动删除最旧的；
- 快照记录的是**切换前**的配置，列表标签以「切换前源」标注当时实际生效的源；
- 点击「快照」→ 某条记录的「恢复」即可回滚，恢复前仍会把当前文件备份为 `.bak`；
- 每条快照还提供「与当前对比」和「对比」（选择另一份快照）两个入口，用只读 diff 查看差异。

### 配置健康检查

其他工具（编辑器插件、脚本、手工编辑）改动 `config.toml` 后，常会留下 cargo 无法解析或会静默失效的残留。点击标题栏的「健康检查」会扫描当前作用域的配置文件，逐条列出问题与处理方式：

- **重复表头**：同一个 `[source.x]` 被重复追加，TOML 无法解析；修复时保留最后一段。
- **残留替换块**：`replace-with` 指向的源已不存在，cargo 会直接报错；修复时移除对应的 `replace-with`，回到默认行为。
- **孤立源段**：`[source.x]` 既没有被 `replace-with` 引用，也不属于 srmgui 管理的源（内置源与登记过的自定义源不受影响）；修复时删除该段。
- **语法错误**：配置已无法解析且无法就地修补时，回退到最近一份可解析的快照，没有快照则使用 `config.toml.bak`。

修复前会自动保存快照与 `.bak` 备份；无法解析又没有可用备份时会明确提示，不会强行覆盖。检查与修复结果都会写入日志面板。

### 自定义源

在「自定义源」标签页填写标识名、名称、Registry URL（说明可选）后点击「添加」。URL 若未带协议，会自动规范化为 `sparse+https://<host>/`。

### 私有 Registry

在「私有 Registry」标签页管理企业私有镜像，和内置源替换是两套独立配置：

- **名称**：对应 `Cargo.toml` 中 `registry = "名称"` 的标识，仅允许字母、数字、连字符与下划线，`crates-io` 为保留名称；
- **Index URL**：支持 `sparse+https://...` sparse 索引，也支持 `https://...`、`ssh://...`、`git@...` 等 git 索引；未带协议时自动补全为 `sparse+https://<host>/`；
- **认证 Token（可选）**：写入 `$CARGO_HOME/credentials.toml` 的 `[registries.<name>]`，列表中显示「已配置 / 未配置」。匿名只读镜像或使用 credential provider 时可留空；
- 索引写入当前作用域的 `config.toml`（全局或项目级），删除时会同步移除对应 registry，并在没有其他配置引用时清理 token。

### 导入 / 导出

- **导出**：将当前所有非内置的自定义源保存为 `.srmgui` 文件（未写扩展名时自动补全），包含标识名、名称、URL、说明。
- **导入**：选择 `.srmgui` 文件，按以下规则合并：
  - 跳过与内置源同名的标识；
  - 跳过已存在的标识；
  - 新增项立即参与测速。

`.srmgui` 是 srmgui 自定义的 TOML 格式（v2）：

```toml
# srmgui 自定义源文件，可通过 srmgui 导入
format = "srmgui.sources"
version = 2

[[source]]
id = "demo-src"
name = "演示源"
url = "sparse+https://demo.example.com/index/"
description = "这是一个演示源"
```

导入时会校验 `format` 与 `version`，`id` / `url` 缺一不可；`name` 缺省时回退为 `id`，`description` 缺省为空。兼容 v1 格式（`name` 作为标识名）。

### 检查更新

应用启动后会在后台检查 GitHub Releases，之后按设置的周期（默认 6 小时）复查一次：

- 发现新版本且存在当前平台的安装包时，窗口右上角显示「发现新版本 x.y.z」；
- 点击后可选择下载方式：
  - **系统下载工具**：用系统默认浏览器 / 下载器打开安装包链接；
  - **应用内下载**：在应用内下载对应平台的安装包，完成后自动启动安装；
- 检查结果、下载与安装过程都会写入日志面板。

### 应用设置

点击工具栏的「设置」可调整运行参数，保存后立即生效（定时任务按新周期重建，并马上重新测速一轮）：

| 分组 | 配置项 | 默认值 | 允许范围 |
| --- | --- | --- | --- |
| 网络 | 超时（秒） | 8 | 1 – 3600 |
| 网络 | 最大重定向 | 5 | 1 – 20 |
| 网络 | 示例 crate | `serde` | 字母、数字、`-`、`_` |
| 网络 | 下载上限（KB） | 512 | 1 – 102400 |
| 周期 | 测速周期（秒） | 180 | 10 – 86400 |
| 周期 | 更新检查周期（秒） | 21600 | 60 – 604800 |
| 快照 | 保留数量（份） | 10 | 1 – 100 |
| 日志 | 单文件上限（MB） | 10 | 1 – 1024 |
| 日志 | 保留滚动数（个） | 5 | 1 – 50 |
| 日志 | 内存日志条数 | 1000 | 100 – 100000 |

- 输入框只接受合法字符，数值越界或无法解析时在顶部给出提示，且不会写入；
- 「恢复默认」把各输入框恢复为默认值，「取消」不产生任何改动；
- 设置保存在 `<应用目录>/srmgui-settings.toml`，仅记录与默认值不同的字段。

## 命令行（CLI）

同一个可执行文件既是 GUI，也提供命令行子命令，与界面共用同一份配置、快照与设置，适合脚本与 CI：

```bash
srmgui status                                   # 作用域、配置文件、当前源
srmgui list                                     # 内置源 + 自定义源，* 为当前生效
srmgui use rsproxy                              # 按默认作用域切换源
srmgui use aliyun --scope project --project .   # 切换项目级 .cargo/config.toml
srmgui probe                                    # 测速当前源；也可 probe rsproxy aliyun 或 --all
srmgui custom list                              # 自定义源列表
srmgui custom add corp --url mirror.example.com --name "公司镜像"
srmgui custom remove corp
srmgui custom export ./sources                  # 自动补全 .srmgui 扩展名
srmgui custom import ./sources.srmgui
srmgui registry add corp --index crates.example.com --token <token>
srmgui registry list
srmgui registry remove corp
srmgui snapshot list                            # 配置快照
srmgui snapshot restore 1                       # 恢复第 1 条，或 latest
srmgui scope                                    # 查看默认作用域
srmgui scope project --project .                # 设置默认作用域（与 GUI 共享）
srmgui doctor --fix                             # 健康检查并修复
srmgui --help
```

- 不带任何参数（或 `srmgui gui`）时打开图形界面；
- `--json` 输出机器可读结果，便于脚本解析（`status` / `list` / `probe` / `custom list` / `snapshot list` / `doctor` 等）；
- `--scope global|project` 临时覆盖作用域，`--project <dir>` 指定项目目录（可为项目内任意路径，自动向上查找 `Cargo.toml`），两者都不会改动 GUI 的默认设置，只有 `srmgui scope` 会持久化；
- 退出码：`0` 成功，`1` 执行失败（含 `probe` 存在失败层、`doctor` 检出问题），`2` 参数错误，`probe` 与 `doctor` 可直接用于 CI 门禁。

## 写入的配置

应用某个源后，`~/.cargo/config.toml` 的内容形如：

```toml
[source.crates-io]
replace-with = "rsproxy"

[source.rsproxy]
registry = "sparse+https://rsproxy.cn/index/"

# ...其余内置源与自定义源...

[net]
git-fetch-with-cli = true
```

选择「官方 crates.io」时不写 `replace-with`，即回到 Cargo 默认行为。

私有 Registry 的索引与 token 分开存放：

```toml
# ~/.cargo/config.toml
[registries.company]
index = "sparse+https://cargo.example.com/index/"
```

```toml
# ~/.cargo/credentials.toml
[registries.company]
token = "******"
```

## 构建与运行

需要 Rust 工具链（edition 2024）。

```bash
cargo run            # 调试运行
cargo build --release
cargo test --bins    # 单元测试
```

### 平台支持

支持 Windows、Linux、macOS 三端，UI 与功能保持一致。

**Windows**：构建脚本会通过 `embed-resource` + `rc.exe` 将 `assets/logo.ico` 嵌入可执行文件，用于窗口 / 任务栏图标（需要 Windows SDK）。

**Linux（Debian/Ubuntu）**：需先安装系统依赖：

```bash
sudo apt update
sudo apt install -y gcc g++ clang pkg-config cmake \
  libfontconfig-dev libfreetype6-dev \
  libwayland-dev libxkbcommon-x11-dev libx11-xcb-dev \
  libwebkit2gtk-4.1-dev libssl-dev libzstd-dev \
  libasound2-dev vulkan-validationlayers libvulkan1
```

其他发行版请安装对应的开发库包。

**macOS**：需安装 Xcode Command Line Tools：

```bash
xcode-select --install
```

应用内标题旁的 logo 通过 `include_bytes!` 内嵌 `assets/logo.png`，不依赖运行时资源目录。

## 项目结构

```
srmgui/
├── assets/           # logo 源文件（svg）、界面用 png、exe 图标 ico、资源脚本 rc
├── build.rs          # Windows 下将 ico 嵌入可执行文件（Linux/macOS 跳过）
├── sources.json      # 内置源清单（启动时在线拉取，离线回退到缓存/内置清单）
├── src/
│   ├── main.rs       # 程序入口与窗口初始化
│   ├── app.rs        # 应用状态（源、地区、缓存、更新等）与配置持久化
│   ├── model.rs      # 状态类型、语言与主题枚举
│   ├── root_view.rs  # 主界面布局、应用/回滚/测速/推荐等交互
│   ├── source_list.rs # 源列表与测速结果展示
│   ├── private_list.rs # 私有 Registry 列表与删除
│   ├── add_form.rs   # 自定义源与私有 Registry 添加表单
│   ├── config_dialog.rs # 写入前预览、快照列表、diff 查看对话框
│   ├── cache_dialog.rs # Cargo 缓存管理对话框
│   ├── settings_dialog.rs # 应用设置对话框：网络、周期、快照与日志参数
│   ├── log_panel.rs  # 日志面板：级别过滤、关键字搜索与诊断包导出
│   ├── ui.rs         # 共用 UI 片段（滚动文字、分隔线、底部滑入面板等）
│   ├── diff.rs       # 行级 LCS diff 算法
│   ├── registry.rs   # 编译期内置源清单与 config.toml 内容生成
│   ├── manifest.rs   # 在线源清单订阅与 srmgui:// 分享链接
│   ├── region.rs     # 出口 IP 地区识别与按地区/测速推荐
│   ├── config_io.rs  # 配置读写、快照、自定义源、私有 Registry 与设置持久化
│   ├── health.rs     # 配置健康检查：重复表头、残留替换块、孤立源段与语法错误
│   ├── cache.rs      # Cargo 缓存扫描、按来源统计与清理
│   ├── diagnostics.rs # 诊断包导出（不含认证凭据）
│   ├── logger.rs     # 内存/文件日志与本地时间戳
│   ├── update.rs     # 检查 GitHub Releases、下载并启动安装包
│   ├── net.rs        # 四层网络探测
│   ├── util.rs       # URL 规范化等小工具
│   └── cli.rs        # 命令行子命令实现
└── Cargo.toml
```

## 技术栈

- Rust 2024 + [GPUI](https://gpui.rs) / [gpui-kit](https://crates.io/crates/gpui-kit)（含 gpui-component）
- `rustls` + `rustls-native-certs` / `webpki-roots` 发起 HTTPS 探测
- `toml` / `toml_edit` 解析与生成配置
- `rust-i18n` 提供 8 种语言的界面文案
- `serde` / `serde_json` / `base64` 处理源清单与分享链接
- `chrono` 生成带时区的本地时间戳

## 关于

- **作者**：liujinpeng
- **邮箱**：[imljp@foxmail.com](mailto:imljp@foxmail.com)
- **GitHub**：[https://github.com/ljporljp/srmgui](https://github.com/ljporljp/srmgui)
- **Crates.io**：[https://crates.io/crates/srmgui](https://crates.io/crates/srmgui)
- **协议**：MIT

本项目（srmgui）及其源代码的所有权归作者 liujinpeng 所有。在 MIT 协议条款下，任何人可自由使用、复制、修改、合并、出版、分发、再许可本软件，但须在所有副本或重要部分中保留上述版权声明与本许可声明。
