# Realtime Quality

独立的 RustPBX 实时语音信箱检测服务。只覆盖：

- 英语（`en`）和印尼语／Bahasa Indonesia（`id`）的流式 ASR + 配置关键词匹配。
- 配置频率的 beep 检测。
- 每条 WebSocket 可选择 ASR、beep 或两者同时运行。

仓库、源码发布包和二进制发布包均不包含模型、录音、测试音频或识别样本报告。关键词只在配置中提供；程序没有内置关键词表。模型从外部目录加载，启动时不自动下载。

## 快速启动：beep

从 CNB Releases 获取匹配平台的压缩包及 `.sha256`，校验后解压。仅发布：

- `x86_64-unknown-linux-gnu`：Linux x86_64，构建基线 Debian 12/glibc 2.36；不是 musl 静态系统包。
- `aarch64-apple-darwin`：Apple Silicon macOS，需满足对应 sherpa-onnx 原生库的系统要求。

```bash
shasum -a 256 -c realtime-quality-VERSION-TARGET.tar.gz.sha256
tar -xzf realtime-quality-VERSION-TARGET.tar.gz
cd realtime-quality-VERSION-TARGET
cp config.example.toml /absolute/path/quality.toml
./realtime-quality --config /absolute/path/quality.toml
curl http://127.0.0.1:8095/health
```

示例配置默认只启用 beep、只报告检测、不主动挂断。无需模型或 GPU。进程加载资源成功后才监听端口，`/health` 返回 `ok`。配置修改后重启服务，不热加载模型。

## 配置

```toml
listen = "127.0.0.1:8095"
# token = "your-secret"   # PBX: Authorization: Bearer your-secret

[detector]
beep_enabled = true
asr_enabled = false
asr_languages = ["en", "id"]
keywords = [
  { language = "en", phrase = "please record your message after the tone" },
  { language = "id", phrase = "layanan pesan suara" },
]
stable_updates = 2
# text_language = "en"   # 可选；只从一个模型输出字幕
frequencies_hz = [1309.0, 660.0]
tolerance_hz = 20.0
min_tone_ms = 160
max_gap_ms = 40
min_dbfs = -35.0
min_energy_ratio = 0.8
inspect_ms = 15000
hangup_on_asr = false
hangup_on_beep = false
```

完整字段见 `config.example.toml`。启用 ASR 时，每个选中的语言必须配置至少一个关键词，且启动配置中必须加载对应模型。关键词比较忽略大小写、标点和连续空白，要求词边界；连续 `stable_updates` 次解码结果命中才报告。它是解码更新次数，不是毫秒数，也不是准确率保证。

`max_gap_ms` 允许短零音频打断 tone；设为 0 禁止间隙。服务保留输入中的零采样，不删除录音静音；这不是 RTP jitter buffer。beep 频率和阈值应依据业务信号配置，不适用于自动识别所有类型的提示音。

PBX `start.parameters` 按顶层 key 覆盖 `[detector]`；数组整体替换。未提供参数继承服务默认。客户端不能改模型路径、线程数、监听地址或鉴权 token。未知参数会被拒绝。

## 安装两个流式模型

只支持 matching streaming transducer 的 encoder、decoder、joiner、tokens 四件套，不接受离线 Whisper 或任意 ONNX 文件替换。

已验证组合：

| 语言 | 来源 | 选择 |
|---|---|---|
| 英语 | [sherpa-onnx streaming Zipformer en 2023-06-26](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26) | chunk 16 / left 128；int8 encoder/joiner、float decoder |
| 印尼语 | [spacewave streaming Zipformer2 id](https://huggingface.co/spacewave/sherpa-onnx-streaming-zipformer2-id) | iter 100000 / avg 15 / chunk 32 / left 256；int8 encoder/joiner、float decoder |

下载脚本只请求八个必要文件，不下载仓库附带的音频。固定模型 revision 和 SHA-256 在 `scripts/models.json`，下载后逐文件验证；重复执行会复用校验通过的文件。需要 Python 3.11+ 和 HTTPS 访问 Hugging Face。

```bash
# 在项目之外选择持久目录；脚本拒绝下载到源码目录内。
python3 scripts/download_models.py --directory /var/lib/realtime-quality/models
# macOS 示例：
# python3 scripts/download_models.py --directory "$HOME/Library/Application Support/realtime-quality/models"
```

也可添加 `--language en` 或 `--language id` 只下载一个语言。模型遵循各自上游许可；下载工具和清单不把模型纳入本项目。更新模型需重新验证，不要只替换某一个文件。

在服务 TOML 中添加（macOS 改为实际绝对路径）：

```toml
[models.en]
encoder = "/var/lib/realtime-quality/models/en/encoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx"
decoder = "/var/lib/realtime-quality/models/en/decoder-epoch-99-avg-1-chunk-16-left-128.onnx"
joiner = "/var/lib/realtime-quality/models/en/joiner-epoch-99-avg-1-chunk-16-left-128.int8.onnx"
tokens = "/var/lib/realtime-quality/models/en/tokens.txt"
num_threads = 2
max_batch_size = 4
max_wait_ms = 5

[models.id]
encoder = "/var/lib/realtime-quality/models/id/encoder-iter-100000-avg-15-chunk-32-left-256.int8.onnx"
decoder = "/var/lib/realtime-quality/models/id/decoder-iter-100000-avg-15-chunk-32-left-256.onnx"
joiner = "/var/lib/realtime-quality/models/id/joiner-iter-100000-avg-15-chunk-32-left-256.int8.onnx"
tokens = "/var/lib/realtime-quality/models/id/tokens.txt"
num_threads = 2
max_batch_size = 4
max_wait_ms = 5
```

然后把 `[detector] asr_enabled` 改为 `true`。双语言各两个内部线程可作为 8 核机器的初始配置；不是容量承诺。当前 provider 固定为 CPU，不需要 GPU。

## RustPBX / wholesale 接入

```toml
[proxy.quality]
enabled = true
url = "ws://127.0.0.1:8095/v1/quality"
sides = "callee"
receive_timeout_secs = 15
headers = []

[proxy.quality.parameters]
asr_enabled = true
beep_enabled = true
asr_languages = ["en", "id"]
hangup_on_asr = true
hangup_on_beep = true
```

参数中的 hangup 开关是本服务策略；PBX 接到 `hangup` 就执行。若只要标记，保持两项为 `false`。服务在另一台机器时，URL 使用 PBX 可达的局域网地址，监听地址也要相应修改。当前服务不直接提供 TLS，可在需要时使用 WebSocket 反向代理。

wholesale 租户地址／参数和路由项启用配置见 `wholesale.example.toml`。覆盖顺序为 PBX 默认 → 租户 → 路由项仅覆盖 enabled。没有 routing profile 级配置；addon 策略修改后重启 wholesale。

## WebSocket v1

端点 `/v1/quality`。PBX 首条 Text：

```json
{"type":"start","version":1,"sample_rate":8000,"call_id":"call-id","session_id":"session-id","side":"callee","direction":"inbound","tenant_id":3,"inbound_trunk_id":16,"outbound_trunk_id":15,"parameters":{"asr_enabled":true,"beep_enabled":true}}
```

其后 Binary 为该方向单声道 **PCM16 little-endian**，无 WAV/RTP 头、无 base64。支持 8000、12000、16000、24000、32000、44100、48000 Hz，每连接采样率固定。双方音频分别建立连接。首条 start 限时 5 秒；单条消息上限 64 KiB。

服务结果不携带 side，由 PBX 根据连接补充。示例：

```json
{"type":"text","text":"Please record your message after the tone","partial":false,"start_ms":0,"end_ms":3100,"lang":"en"}
{"type":"calltrace","kind":"quality","code":"quality.voicemail_detected","severity":"info","message":"Voicemail detected","detail":{"method":"asr","language":"en","phrase":"please record your message after the tone","matched_text":"PLEASE RECORD YOUR MESSAGE AFTER THE TONE","end_ms":3100}}
{"type":"hangup","reason":"quality.voicemail_detected","detail":{"method":"asr","language":"en","phrase":"please record your message after the tone","matched_text":"PLEASE RECORD YOUR MESSAGE AFTER THE TONE","end_ms":3100}}
```

beep 同样报告 `quality.voicemail_detected`，detail 为 `{"method":"beep","detection":{"frequency_hz":1309.0,"start_ms":540,"end_ms":700,"tone_ms":160,"energy_ratio":0.99}}`。无观察时不报告，不需要最终判断。

服务每 10 秒发 WebSocket Ping；PBX 自动 Pong，默认 15 秒无入站消息关闭质检连接。任一方 Close、网络断开或 inspect_ms 到期，只结束质检，不挂断电话。只有显式 hangup 才请求电话挂断。

在核心 `rustpbx_call_records.metadata.trace` 查 `code="quality.voicemail_detected"`；最终 End 的 `detail.source="realtime_quality"` 和 `detail.reason` 记录服务挂断来源。wholesale 金额通过 `call_id` 关联 `wholesale_cdrs`，短通话仍正常计费。UI 在通话详情的 Quality / End 时间线查看；没有新增 voicemail 顶层列。

## 结构和内存

- `main.rs`：clap、读配置、初始化、监听及退出；`lib.rs`：服务装配。
- `web.rs`：每连接一个异步收发循环，处理音频、worker 结果、心跳和关闭。
- `audio.rs`：对齐的完整 PCM 块用 bytemuck 借用 `&[i16]`；未对齐或跨消息的块复制到可复用的 `Vec<i16>`。`i16::from_le` 在两个发布架构上为恒等操作，归一化使用 dasp_sample。
- Rubato 在每连接中复用状态和输入缓冲，直接写入最终 100 ms f32 输出块；16 kHz 不重采样。输出 `BytesMut` split/freeze 后浅克隆给 worker，没有中间输出块复制。
- beep 使用 20 ms 窗口；ASR 使用 100 ms 输入块。同一语言共享一个 recognizer/worker，每连接拥有独立 OnlineStream 和关键词状态。语言之间不混合 batch。
- 每语言入口 channel 64、每连接每语言待处理 2 块、结果队列 32、连接上限 256。队列满会停止该连接 ASR 并报告 `quality.asr_failed`，beep 可继续。它不是无界后台推理队列。

关闭会丢弃未满块的尾部，不补零或强制制造最终识别结果。原生推理已进入一次 decode 时不能强行中断。

## Linux 常驻部署

将解压内容放到 `/opt/realtime-quality`，配置放到 `/etc/realtime-quality/config.toml`，模型放到 `/var/lib/realtime-quality/models`。创建专用无登录账户，使其可读配置和模型，然后安装 `deploy/realtime-quality.service`：

```bash
sudo useradd --system --home-dir /var/lib/realtime-quality --shell /usr/sbin/nologin realtime-quality
sudo install -d -o realtime-quality -g realtime-quality /var/lib/realtime-quality
sudo install -d -m 0750 /etc/realtime-quality
sudo install -m 0640 -o root -g realtime-quality config.example.toml /etc/realtime-quality/config.toml
# 编辑配置、下载模型并确保服务账户可读，然后：
sudo install -m 0644 deploy/realtime-quality.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now realtime-quality
journalctl -u realtime-quality -f
```

前台 macOS：`./realtime-quality --config /absolute/path/quality.toml`，Ctrl-C 退出。若配置 token，PBX headers 必须一致。不要把真实 token 或本机模型路径提交到 Git。

## 编译、测试、发布 CI

需要 Rust stable（edition 2024）、C/C++ 链接工具；macOS 安装 Xcode Command Line Tools。sherpa-onnx-sys 自动获取匹配目标的原生静态库；需要访问其 GitHub Releases。离线构建可预置 `SHERPA_ONNX_ARCHIVE_DIR` 或 `SHERPA_ONNX_LIB_DIR`（见上游 crate 文档）。静态链接 sherpa/ONNX，不表示整个 Linux 系统依赖均静态链接。

```bash
cargo test --locked
cargo run -- --config /absolute/path/quality.toml
bash scripts/package.sh aarch64-apple-darwin v0.1.0
# 在 Linux x86_64 主机：
# bash scripts/package.sh x86_64-unknown-linux-gnu v0.1.0
```

普通测试仅在内存生成信号，不写音频文件、不下载模型。可选真实模型 WebSocket 测试读取外部 `QUALITY_TEST_CONFIG`、`QUALITY_TEST_MODEL_DIR`、`QUALITY_TEST_LANGUAGE` 和 `QUALITY_TEST_WAV`；模型和录音必须位于项目之外。该测试验证流式输入，不把文件一次性提交给识别器。

`.cnb.yml`：main push 跑 Linux 测试和资产检查；版本 tag push 分别原生构建 Linux x86_64 与 macOS arm64，上传 tar.gz 和 SHA-256。发布脚本先创建 draft，两个平台及校验文件齐全后才发布。版本标签必须和 Cargo.toml 版本对应（允许 `v` 前缀和预发布后缀）。

**Mac CI 要求 CNB 组织自托管 runner，标签 `[mac, arm64]`**，已安装 Rust、Python 3.11+ 和 Xcode CLI。CNB 托管节点提供 Linux，不提供托管 macOS；本仓库不会在开发机擅自安装后台 runner。若组织使用其他标签，请修改 `.cnb.yml`。配置方式见 [CNB build nodes](https://docs.cnb.cool/en/build/build-node.html)。

```bash
git tag v0.1.0
git push origin v0.1.0
```

`dist/` 使用白名单打包；CI 的 `scripts/check_assets.py` 拒绝 Git 中的模型、音频、本机配置和构建产物。发布包只包含可执行文件、配置示例、模型下载清单／脚本、README 和 systemd 单元。
