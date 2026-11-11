# Realtime Quality

源码与发布：[GitHub / miuda-ai/realtime-quality](https://github.com/miuda-ai/realtime-quality)。

独立的 RustPBX 实时语音信箱检测服务。只覆盖：

- 英语（`en`）和印尼语／Bahasa Indonesia（`id`）的流式 ASR + 配置关键词匹配。
- 配置频率的 beep 检测。
- 每条 WebSocket 可选择 ASR、beep 或两者同时运行。

仓库、源码发布包和二进制发布包均不包含模型、录音、测试音频或识别样本报告。关键词只在配置中提供；程序没有内置关键词表。模型从外部目录加载，启动时不自动下载。

## 快速启动：beep

从 [GitHub Releases](https://github.com/miuda-ai/realtime-quality/releases) 获取匹配平台的压缩包及 `.sha256`，校验后解压。仅发布：

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

示例配置默认只启用 beep、只报告检测、不主动挂断。跨机器部署时，将 `listen` 改为服务内网地址或 `0.0.0.0:8095`。无需模型或 GPU。进程加载资源成功后才监听端口，`/health` 返回 `ok`。配置修改后重启服务，不热加载模型。

## 配置

服务配置文件只包含进程资源：监听地址、可选认证、模型文件及每个模型的线程和批处理参数，见 `config.example.toml`。不再支持 `[detector]`。模型启动时加载，共享给连接；请求不能修改模型路径、线程数、监听地址或认证。

```toml
listen = "127.0.0.1:8095"
# token = "your-secret"   # PBX: Authorization: Bearer your-secret
# [models.id] / [models.en] 的完整配置见下面模型安装部分。
```

检测设置由每条 WebSocket 的首条 `start` 消息携带，下面是它的 `parameters` 对象：

```json
{
  "asr_enabled": true,
  "asr_languages": ["id", "en"],
  "beep_enabled": true,
  "keywords": [
    {"language": "id", "phrase": "layanan pesan suara"},
    {"language": "id", "sequence": ["rekam", "setelah mendengar nada"], "max_gap_words": 10},
    {"language": "en", "phrase": "forwarded to voicemail"},
    {"language": "en", "phrase": "forwarded to voice mail"}
  ],
  "stable_updates": 2,
  "frequencies_hz": [1309, 660],
  "inspect_ms": 15000,
  "hangup_on_detection": false
}
```

`StartMessage` 直接将 `parameters` 反序列化为 `InspectionParameters`，不经过通用 JSON 值的二次转换。省略 parameters 或传入 `{}` 使用内置默认值；显式 `null`、字段类型错误或未知字段会被拒绝。serde enum 检查 type、side 和 direction；`validator::Validate` 的注解检查版本、数值范围、列表长度和嵌套关键词，跨字段关系使用 schema 校验。任务初始化统一调用 `start_message.validate()`，通过后创建 detector/handler 和注册 ASR stream，解析层不创建运行资源。显式提供的字段即使对应 detector 关闭也需要满足基本取值约束；两种 detector 都关闭时正常结束质检连接。省略字段使用内置默认值，不继承其它连接。缺省为 beep 开启、ASR 关闭、观察 15 秒、不挂断；ASR 启用时必须选择已加载的语言，并为每种语言提供关键词。`keywords` 是本连接的完整规则列表。未知字段会被拒绝。

规则之间是 OR；每条规则只能提供 `phrase` 或 `sequence`。`sequence` 包含 2–8 段按顺序出现的短句，必须指定 `max_gap_words`（0–100），限制每对相邻短句间允许的词数，0 表示紧邻。不同位置的重复锚点均会检查，短句不重叠。不跨语句累积锚点；每次重新检查当前 ASR 文本，以支持 partial 修正。

关键词比较忽略大小写、标点和连续空白，要求词边界。同一规则连续 `stable_updates` 次解码更新满足才报告，默认 2；partial 和相同文本的新解码更新都计数，某次不满足则清零。它不是毫秒数或准确率保证。顺序规则的 trace `phrase` 用 ` … ` 连接各段锚点。

`hangup_on_detection` 对 ASR 和 beep 命中统一生效：先记录 trace，开启时再请求挂断，发送成功后立即结束质检并进行 WebSocket Close 握手，不等待 PBX 先关闭。旧的 `hangup_on_asr` / `hangup_on_beep` 已移除，请迁移 PBX 策略参数。

`inspect_ms` 从有效 start 参数解析完成时计时，包含等待 worker 接纳和处理的时间；到期结束质检并关闭 WebSocket，无音频也会到期，不产生“真人”或挂断事件。发送等待同样受观察截止时间限制；Close 握手另有最多 1 秒。已进入原生推理的工作不能强制中断，但其结果不再上报，worker 随后清理状态。

beep 默认参数：`tolerance_hz=20`、`min_tone_ms=160`、`max_gap_ms=40`、`min_dbfs=-35`、`min_energy_ratio=0.8`。`max_gap_ms` 允许短零音频打断 tone；设为 0 禁止间隙。服务不删除录音静音；这不是 RTP jitter buffer。频率和阈值应依据业务信号配置。

开启 ASR 后，所选语言的转录文本自动作为 text 事件返回，通过 `lang` 区分；不再提供 `text_language` 参数。每次独立检测都会上报 trace，不限制整个连接的上报次数。

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

然后在连接的 `start.parameters` 中设置 `asr_enabled=true` 并提供关键词。双语言各两个内部线程可作为 8 核机器的初始配置；不是容量承诺。当前 provider 固定为 CPU，不需要 GPU。

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
hangup_on_detection = false
inspect_ms = 15000
keywords = [
  { language = "en", phrase = "forwarded to voicemail" },
  { language = "en", phrase = "forwarded to voice mail" },
  { language = "id", phrase = "layanan pesan suara" },
]
```

参数中的 hangup 开关是本服务策略；PBX 接到 `hangup` 就执行。若只要标记，保持 `hangup_on_detection=false`。服务在另一台机器时，URL 使用 PBX 可达的局域网地址，监听地址也要相应修改。当前服务不直接提供 TLS，可在需要时使用 WebSocket 反向代理。

wholesale 控制台中创建质检策略，填写服务地址和上述 parameters；租户和路由项选择策略、禁用或留空继承，具体覆盖一般配置。PBX 默认配置示例见 `wholesale.example.toml`。

## WebSocket v1

端点 `/v1/quality`。PBX 首条 Text：

```json
{"type":"start","version":1,"sample_rate":8000,"call_id":"call-id","session_id":"session-id","side":"callee","direction":"inbound","tenant_id":3,"inbound_trunk_id":16,"outbound_trunk_id":15,"parameters":{"asr_enabled":true,"asr_languages":["en","id"],"beep_enabled":true,"hangup_on_detection":false,"keywords":[{"language":"en","phrase":"after the tone"},{"language":"id","phrase":"layanan pesan suara"}]}}
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

```text
src/
├── main.rs                 # CLI、配置加载、日志和退出信号
├── lib.rs                  # 对外导出 QualityService 和 config 配置模块
├── config.rs               # 监听、认证与共享模型配置
├── service/
│   ├── mod.rs              # QualityService：路由、准入、启动与退出
│   ├── recognition_task.rs # RecognitionTask：每连接任务及状态
│   ├── audio.rs            # AudioProcessor：音频转换、重采样和分块
│   └── protocol.rs         # PBX start 和对外质检事件
└── recognition/
    ├── mod.rs              # 共享识别结果、检测证据和事件
    ├── parameters.rs       # 每连接 InspectionParameters、KeywordRule、默认值和校验
    ├── beep.rs             # BeepDetector：音频直接产生检测结果
    └── asr/
        ├── mod.rs          # 模型加载、RecognitionStream 和 AsrResultHandler 接口
        ├── worker.rs       # 每语言 worker、注册句柄和批量推理
        └── keywords.rs     # KeywordMatcher：接口实现、短句/顺序匹配和连续确认
```

`recognition/mod.rs` 集中定义 `RecognitionResult`、`RecognitionEvent`、`BeepDetection` 和 `VoicemailEvidence`。ASR worker 只生成识别文本；连接通过 `RecognitionHandle` 接收结果，再交给 `Box<dyn AsrResultHandler>`。接口定义在 `asr/mod.rs`，当前 `keywords.rs` 的 `KeywordMatcher` 直接实现它，按连接配置匹配短句或顺序规则并生成检测证据。替换判断方法时实现新的 handler 并在连接初始化时选择它，无需修改模型、worker 或音频通道。每连接一个 handler，内部隔离各语言的确认状态；没有额外 policy 包装、任务或通道。

字幕由 `RecognitionTask` 按语言独立发送和去重，不属于 handler 的检测职责。beep 直接生成检测结果，不经过 ASR handler。对外消息格式留在 `service/protocol.rs`；识别实现不依赖服务或 WebSocket 协议。

`main.rs` 负责 CLI、配置读取、日志、监听地址和退出信号，通过 `QualityService::prepare()` 和 `serve()` 启动服务。库只公开 `QualityService` 和 `config` 配置模块，检测器和识别实现保持内部可见。

`RecognitionTask` 统一处理音频、心跳、检测结果去重、事件发送和挂断策略。每种语言共享一个 worker 和 recognizer，每连接一个独立 `RecognitionStream`；worker 不做关键词或挂断判断，语言之间不混合 batch。

`AudioProcessor` 复用重采样状态和输入缓冲；Rubato 直接写入最终 100 ms f32 输出块，16 kHz 不重采样。输出 `BytesMut` split/freeze 后浅克隆给语言 worker。beep 使用 20 ms 窗口，只有配置 ASR 时才需要模型。

音频使用每连接、每语言独立的 `rtrb` SPSC ring buffer，容量为 5 个 100 ms `Bytes` 块（最多排队 500 ms，不包括 Sherpa 内部已接收的音频）。连接持有 Producer，worker 持有 Consumer 和结果 sender。满时直接丢弃新音频块，不覆盖旧块、不阻塞、不报错、不结束识别；有空位后自动继续接收。worker 每轮最多从每条连接取五个块，再排队批量推理。丢弃的音频不补零、不重播；ASR 时间戳按实际送入模型的音频计数，丢块后不能视为原始通话的精确时间位置。观察期限仍按墙钟时间执行，不因丢块或积压延长。

注册使用独立的有界控制通道，将 Consumer 和结果 sender 直接交给 worker，不使用 oneshot 确认，也没有共享过载标志。注册队列满时跳过该质检连接，正常关闭 WebSocket，不挂断电话；双语言中途注册失败时，同一结果 receiver 被释放，已注册的 worker 会清理对应状态。该队列限制待注册数量，不代表活跃连接上限。

连接退出时关闭结果 receiver；worker 在注册和处理时检查 sender 是否关闭，删除对应 stream 和待处理音频，不依赖关闭命令。音频、注册、连接退出和进程关闭通过 Crossbeam `Parker/Unparker` 唤醒 worker：每 worker 一个 Parker，连接持有其 Unparker 克隆，不持有线程句柄；通知会合并且不携带音频；空闲时仍每 100 ms 检查一次清理；正在执行的原生 decode 需要先返回。结果通道仍有界，结果接收端关闭或无法接收时释放相应识别流。

服务退出时通知所有连接，等待连接释放和有时限的 WebSocket Close 握手，再停止并 join worker 线程。原生推理无法强行中断，因此不承诺 native decode 卡死时的硬退出时限。关闭丢弃未满块的尾部，不补零或强制制造最终识别结果。

## Linux 常驻部署

将解压内容放到 `/opt/realtime-quality`，配置放到 `/etc/realtime-quality/config.toml`，模型放到 `/var/lib/realtime-quality/models`。创建专用无登录账户，使其可读配置和模型，然后安装 `deploy/realtime-quality.service`：

```bash
sudo useradd --system --home-dir /var/lib/realtime-quality --shell /usr/sbin/nologin realtime-quality
sudo install -d -o realtime-quality -g realtime-quality /var/lib/realtime-quality
sudo install -d -m 0750 -o root -g realtime-quality /etc/realtime-quality
sudo install -m 0640 -o root -g realtime-quality config.example.toml /etc/realtime-quality/config.toml
# 编辑配置、下载模型并确保服务账户可读，然后：
sudo install -m 0644 deploy/realtime-quality.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now realtime-quality
journalctl -u realtime-quality -f
```

前台 macOS：`./realtime-quality --config /absolute/path/quality.toml`，Ctrl-C 退出。若配置 token，PBX headers 必须一致。不要把真实 token 或本机模型路径提交到 Git。

## 日志

服务使用 `tracing` 输出带时间、级别和字段的 stderr 日志；默认 `info`。通过 `RUST_LOG` 调整过滤，例如：

```bash
RUST_LOG=info,realtime_quality=debug ./realtime-quality --config /etc/realtime-quality/config.toml
```

`ensure!` / `bail!` 负责返回错误，不直接打印。启动失败由 main 返回 anyhow 错误链，打印至 stderr 并以非零状态退出；模型错误包含语言上下文。WebSocket 错误在连接边界记录，解析 start 后附带 call_id、session_id、side。正常关闭为 debug；音频 ring buffer 满时静默丢弃新块。不逐帧记录音频或转录文本，也不打印配置 token。systemd 部署使用 `journalctl -u realtime-quality -f` 查看日志；库本身不安装全局 subscriber，由宿主控制日志。

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

`.github/workflows/release.yml`：main push、PR 或手动触发时，在 Linux x86_64 和 macOS arm64 上测试及打包，并上传保留 7 天的 Actions artifacts。`v*` tag 触发相同构建，两个平台都通过后，统一上传 tar.gz 和 SHA-256 到 GitHub Releases，再发布 draft。版本标签必须和 Cargo.toml 版本对应（允许预发布后缀）。

Linux 使用 GitHub 托管 Ubuntu runner 中的 Debian 12 构建容器；Mac 使用托管 `macos-15` arm64 runner，无需自托管机器。仓库 Actions 需要允许托管 runner；发布 job 使用自带 `GITHUB_TOKEN` 的 `contents: write` 权限，不需要另配个人 token。见 [GitHub runner 文档](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/choose-the-runner-for-a-job)。

```bash
git tag v0.1.0
git push origin v0.1.0
```

`dist/` 使用白名单打包；CI 的 `scripts/check_assets.py` 拒绝 Git 中的模型、音频、本机配置和构建产物。发布包只包含可执行文件、配置示例、模型下载清单／脚本、README 和 systemd 单元。

检测 trace 不设整个连接的次数上限：每段独立 beep、每种语言每个语句的首次确认命中分别上报；同一句 ASR 的重复 partial 不重复报告，持续的一段 beep 也只报告一次。配置 `hangup_on_detection = false` 可持续观察直到 `inspect_ms` 到期，不主动挂断通话；开启时发送一次 trace 和 hangup 后立即结束质检，不再发送后续文本或检测事件。
