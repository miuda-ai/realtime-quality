# Realtime Quality v0.2.0：部署与 RustPBX Wholesale 集成

本手册按部署顺序组织：**Release / 源码 → 模型 → 基础配置 → RustPBX 全局接入 → 协议 → 详细选项 → 数据库迁移**。

默认例子同时启用英语 `en`、印尼语 / Bahasa Indonesia `id` 和 1309 / 660 Hz beep，观察 20 秒，只报告检测，**不挂断电话**。`inspect_ms` 的程序内置默认是 15000；示例显式设为 20000，以覆盖较长提示语。

- [v0.2.0 Release](https://github.com/miuda-ai/realtime-quality/releases/tag/v0.2.0)
- [Wholesale 控制台操作 PDF](https://github.com/miuda-ai/realtime-quality/releases/download/v0.2.0/realtime-quality-console-zh.pdf)：只讲创建策略、租户 / 路由选择和查看通话轨迹截图。
- 本文所有密钥都是占位值；源码、发布包和文档都不包含模型、录音或测试音频。

## 1. 使用正式 Release，或从源码构建

### 1.1 直接使用 Release 二进制

无需安装 Rust。支持 Linux x86_64 和 Apple Silicon macOS arm64。Linux 包基于 Debian 12 / glibc 2.36 构建，不是 musl 全静态包；macOS 在原生 arm64 环境运行。

以下命令先按当前操作系统选择目标，然后下载正式 Release 的压缩包和校验文件：

```bash
VERSION=v0.2.0
case "$(uname -s):$(uname -m)" in
  Linux:x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Darwin:arm64) TARGET=aarch64-apple-darwin ;;
  *) echo "Supported: Linux x86_64 or macOS arm64"; exit 1 ;;
esac
PACKAGE="realtime-quality-${VERSION}-${TARGET}"
BASE="https://github.com/miuda-ai/realtime-quality/releases/download/${VERSION}"
curl -fL --retry 3 "$BASE/$PACKAGE.tar.gz" -o "$PACKAGE.tar.gz"
curl -fL --retry 3 "$BASE/$PACKAGE.tar.gz.sha256" -o "$PACKAGE.tar.gz.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "$PACKAGE.tar.gz.sha256"
else
  shasum -a 256 -c "$PACKAGE.tar.gz.sha256"
fi
tar -xzf "$PACKAGE.tar.gz"
cd "$PACKAGE"
./realtime-quality --version
```

预期版本为 `realtime-quality 0.2.0`。也可在 Release 页面手动下载。解压目录自带 `scripts/download_models.py`、`scripts/models.json`、配置示例和 `deploy/`，接着执行第 2、3 节；不是直接用空配置启动双语言识别。

### 1.2 从源码构建同一版本

需要 Rust stable（edition 2024）、C/C++ 链接工具。macOS 需要 Xcode Command Line Tools；Linux 需要常规编译工具链。Sherpa 构建依赖会下载原生库，因此构建时需访问 GitHub。

```bash
git clone --branch v0.2.0 --depth 1 \
  https://github.com/miuda-ai/realtime-quality.git
cd realtime-quality
cargo test --locked
cargo build --release --locked
./target/release/realtime-quality --version
```

后文的 `./realtime-quality` 指 Release 解压目录内的程序；源码方式将它替换为 `./target/release/realtime-quality`。其他脚本路径在两种方式下相同。

v0.2.0 相比旧版移除了 `[detector]` 和旧检测字段。不要混用 v0.1.0 二进制与本手册的配置；升级前按第 6 节迁移 per-connection 参数。

## 2. 下载两个流式模型

需要 **Python 3.11+**。从上节的解压目录或源码根目录运行。模型放在项目之外，以下默认路径不需要 root：

```bash
MODEL_DIR="$HOME/.local/share/realtime-quality/models"
python3 scripts/download_models.py --directory "$MODEL_DIR"
```

默认下载 `en`、`id` 两种语言，每种包含 encoder、decoder、joiner、tokens 四个文件。固定 revision 和 SHA-256 在 `scripts/models.json`；下载后逐文件校验，重复执行复用已校验文件，不下载上游样本音频。仅需一种语言可加 `--language en` 或 `--language id`。

| 语言 | 模型 | 本服务使用的文件组合 |
| --- | --- | --- |
| 英语 `en` | [streaming Zipformer en 2023-06-26](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26) | chunk 16 / left 128；int8 encoder、float decoder、int8 joiner |
| 印尼语 `id` | [streaming Zipformer2 id](https://huggingface.co/spacewave/sherpa-onnx-streaming-zipformer2-id) | iter 100000 / avg 15 / chunk 32 / left 256；int8 encoder、float decoder、int8 joiner |

必须使用同一套 streaming transducer 的四件套，不能只替换一个文件或换成任意 ONNX / 离线 Whisper。当前 CPU provider 不需要 GPU；模型不随服务启动自动下载。

## 3. 基础配置与启动

### 3.1 创建包含双模型的配置

在同一终端中保留 `MODEL_DIR`，设置你自己的服务密钥。以下未加引号的 heredoc 会把变量展开成 TOML 中的实际值；程序本身不做环境变量替换。

```bash
QUALITY_TOKEN='YOUR_SERVICE_TOKEN'
cat > quality.toml <<EOF
listen = "127.0.0.1:8096"
token = "$QUALITY_TOKEN"

[models.en]
encoder = "$MODEL_DIR/en/encoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx"
decoder = "$MODEL_DIR/en/decoder-epoch-99-avg-1-chunk-16-left-128.onnx"
joiner = "$MODEL_DIR/en/joiner-epoch-99-avg-1-chunk-16-left-128.int8.onnx"
tokens = "$MODEL_DIR/en/tokens.txt"
num_threads = 2
max_batch_size = 4
max_wait_ms = 5

[models.id]
encoder = "$MODEL_DIR/id/encoder-iter-100000-avg-15-chunk-32-left-256.int8.onnx"
decoder = "$MODEL_DIR/id/decoder-iter-100000-avg-15-chunk-32-left-256.onnx"
joiner = "$MODEL_DIR/id/joiner-iter-100000-avg-15-chunk-32-left-256.int8.onnx"
tokens = "$MODEL_DIR/id/tokens.txt"
num_threads = 2
max_batch_size = 4
max_wait_ms = 5
EOF
chmod 600 quality.toml
```

`YOUR_SERVICE_TOKEN` 必须替换；PBX 的 `Authorization` 值要与这里相同。若确实不使用认证，可删除 `token` 行，同时在 PBX 使用 `headers=[]`。

### 3.2 启动并检查

```bash
./realtime-quality --config ./quality.toml
```

另一个终端执行：

```bash
curl -fsS http://127.0.0.1:8096/health
```

模型加载成功后才开始监听，健康检查返回 `ok`。此时只完成了进程资源准备，**每个呼叫检测什么由 PBX 的 `parameters` 决定**。不传检测参数时默认只启用 beep，不会自动启用已加载的 ASR 模型。

同机部署可使用回环地址；跨机器应将 `listen` 改为服务内网地址或 `0.0.0.0:8096`，PBX URL 同步修改为可达内网地址。容器中的 `127.0.0.1` 是容器自身。服务直接提供 `ws://`；需要 `wss://` 时使用支持 WebSocket 的 TLS 反向代理。

### 3.3 日志、重启和 Linux 常驻运行

```bash
RUST_LOG=info,realtime_quality=debug \
  ./realtime-quality --config ./quality.toml
```

日志写 stderr，直接携带已解析的 `call_id`、`session_id`、`side`。这与持久化到 CDR 的 `calltrace` 事件不同。监听、token、模型路径或线程配置变更需重启；识别规则在后续每次连接的 start 中传入，不需要重启模型。

Linux 可使用包内 `deploy/realtime-quality.service`。先把程序安装到 `/opt/realtime-quality/realtime-quality`，配置到 `/etc/realtime-quality/config.toml`，模型放在 `/var/lib/realtime-quality/models` 并更新 TOML 绝对路径。配置中的模型不能留在 `/home` 下：该单元启用了 `ProtectHome=true`。

创建或使用专用 `realtime-quality` 用户/组，使其可读配置和模型；完成上述准备后：

```bash
sudo install -m 0644 deploy/realtime-quality.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now realtime-quality
journalctl -u realtime-quality -f
```

systemd 部署是 Linux 操作说明；Mac 上使用前台命令，Ctrl-C 退出。

## 4. 集成到 RustPBX / Wholesale

### 4.1 先用主配置全局启用

以下与发布包 `wholesale.example.toml` 相同，合并进 **运行 Wholesale 程序的主配置文件**。若已有同名 TOML 表，在原表中修改，不要重复声明。把服务 URL 和 token 换成实际值。

```toml
# Merge these tables into the wholesale application's existing config.
# Replace the URL and token. Do not duplicate an existing [proxy.quality] table.
[proxy.quality]
enabled = true
url = "ws://127.0.0.1:8096/v1/quality"
sides = "callee"
receive_timeout_secs = 15
headers = [["Authorization", "Bearer YOUR_SERVICE_TOKEN"]]

[proxy.quality.parameters]
asr_enabled = true
asr_languages = ["en", "id"]
beep_enabled = true
frequencies_hz = [1309.0, 660.0]
stable_updates = 2
inspect_ms = 20000
hangup_on_detection = false
keywords = [
  { language = "en", phrase = "forwarded to voicemail" },
  { language = "en", phrase = "forwarded to voice mail" },
  { language = "en", sequence = ["please record", "after the tone"], max_gap_words = 4 },
  { language = "id", phrase = "layanan pesan suara" },
  { language = "id", sequence = ["rekam", "setelah mendengar nada"], max_gap_words = 10 },
]
```

该例明确 `enabled=true`，所以即使 Wholesale 没有定义任何具体策略，也能走全局质检。为验证全局配置，租户和匹配的路由条目都必须留空继承；已有禁用或具体策略会覆盖它。主配置修改后重启 Wholesale。

确保媒体通过 PBX：主配置已有 `[proxy]` 中设置 `media_proxy="all"`，测试租户关闭媒体旁路。质检在接听后自动开始，不需要另发启动命令，也不以 183 early media 为本例检测起点。`callee` 是被叫产生的音频，不是“发送给被叫”的混音。

若要录音和本地信令归档，沿用现有独立配置；质检不会自动代替录音设置：

```toml
[recording]
enabled = true
type = "local"
auto_start = true
auto_start_at = "media"
path = "/var/lib/rustpbx/recordings"

[callrecord]
type = "database"

[sipflow]
type = "local"
root = "/var/lib/rustpbx/sipflow"
flush_count = 1
flush_interval_secs = 1
```

目录需要 PBX 用户可写，租户录音选项也应启用。这里是已有应用的配置片段，不包括数据库连接、SIP 监听、trunk 和计费配置。

### 4.2 再在 Wholesale 指定具体策略

操作截图见独立 PDF，技术规则如下：

1. “VoIP 批发 → 质检策略”新建策略，填写 `url`、`sides`、接收超时、headers 和 parameters。
2. 租户“编辑 → 设置 → 质检策略”可选择该策略。
3. 路由配置的**路由条目**末尾也可选择策略，它覆盖租户；条目上方选择出站 trunk。策略不是直接绑定在 trunk 实体，也不是 routing profile 级别。
4. 保存后，在批发仪表盘“重载批发配置”，应用到新呼叫。

| 选择 | 含义 |
| --- | --- |
| 空值 | 继承一般配置；SQL NULL 或 `{"policy_id":null}` |
| 禁用 | `{"policy_id":0}`；内置常量，不插入 id=0 的策略记录 |
| 自定义策略 | `{"policy_id":正整数}`；启用该策略 |

先选最具体层级：**路由条目 → 租户 → PBX 默认**。路由明确禁用可覆盖租户启用；路由选择策略也可覆盖租户禁用。选中路由策略不会再叠加租户策略。

所选策略覆盖 PBX 默认时：`url/sides/receive_timeout_secs/enabled` 覆盖标量；`headers` 整体替换；`parameters` 按顶层 key 覆盖，列表整体替换，不递归合并。因此建议每份策略填写完整 parameters，避免旧全局参数残留。

控制台“服务参数”是 JSON，而不是 TOML。上面全局示例对应的对象如下：

```json
{
  "asr_enabled": true,
  "asr_languages": [
    "en",
    "id"
  ],
  "beep_enabled": true,
  "keywords": [
    {
      "language": "id",
      "phrase": "layanan pesan suara"
    },
    {
      "language": "id",
      "sequence": [
        "rekam",
        "setelah mendengar nada"
      ],
      "max_gap_words": 10
    },
    {
      "language": "en",
      "sequence": [
        "please record",
        "after the tone"
      ],
      "max_gap_words": 4
    },
    {
      "language": "en",
      "phrase": "forwarded to voicemail"
    },
    {
      "language": "en",
      "phrase": "forwarded to voice mail"
    }
  ],
  "stable_updates": 2,
  "frequencies_hz": [
    1309,
    660
  ],
  "inspect_ms": 20000,
  "hangup_on_detection": false
}
```

### 4.3 headers 与 parameters 的区别

| 配置 | 发送时机 | 作用 |
| --- | --- | --- |
| `headers` | WebSocket HTTP Upgrade 请求 | 网络认证等请求头；示例 `[["Authorization","Bearer YOUR_SERVICE_TOKEN"]]` |
| `parameters` | 连接建立后的首条 JSON Text `start` | 单次音轨检测规则，如关键词、语言、beep、观察时长和是否挂断 |

PBX 不解释本服务的关键词规则，只负责传递 JSON。不要把 token 放在 URL 或 parameters 中。修改 headers / parameters 影响新连接，不会中途重配置已建立的连接。

PBX 端点选项：`enabled` 默认 false，开启即接听后自动启动；`sides` 可为 caller/callee/both，核心缺省 both，本文建议 callee；`receive_timeout_secs` 默认 15 秒且必须大于 0，它是服务入站消息超时，不是 inspect_ms。

## 5. 服务如何工作：请求、音频与返回事件

### 5.1 每个音轨一个 WebSocket

```text
Caller / Callee RTP
        ↓
RustPBX：解码，各 leg 单独取得 PCM
        ↓  HTTP Upgrade + headers
WebSocket：先 Text start，再 Binary PCM16LE
        ↓
服务：必要时重采样到 16 kHz
        ├─ BeepDetector → 检测证据
        └─ 100 ms 块 → 共享语言 worker / 独立 stream
                         → ASR 文本 → 关键词匹配
        ↓
Text JSON：text / calltrace / hangup
        ↓
RustPBX：转发文本 / 保存轨迹 / 执行挂断
```

每语言模型进程启动时加载一次；每连接有独立识别和匹配状态。两语言各使用自己的 recognizer/worker，不是模型自动识别语言后只运行其中一个。beep 和 ASR 可单独或同时启用。

### 5.2 PBX 发送 start

端点 `/v1/quality`。以下是一条完整、可解析的 UTF-8 JSON Text 示例；路由 ID 按实际呼叫替换，缺失时可以省略。

```json
{
  "type": "start",
  "version": 1,
  "sample_rate": 8000,
  "call_id": "demo-call",
  "session_id": "demo-session",
  "side": "callee",
  "direction": "inbound",
  "tenant_id": 24,
  "inbound_trunk_id": 84,
  "outbound_trunk_id": 83,
  "parameters": {
    "asr_enabled": true,
    "asr_languages": [
      "en",
      "id"
    ],
    "beep_enabled": true,
    "keywords": [
      {
        "language": "id",
        "phrase": "layanan pesan suara"
      },
      {
        "language": "id",
        "sequence": [
          "rekam",
          "setelah mendengar nada"
        ],
        "max_gap_words": 10
      },
      {
        "language": "en",
        "sequence": [
          "please record",
          "after the tone"
        ],
        "max_gap_words": 4
      },
      {
        "language": "en",
        "phrase": "forwarded to voicemail"
      },
      {
        "language": "en",
        "phrase": "forwarded to voice mail"
      }
    ],
    "stable_updates": 2,
    "frequencies_hz": [
      1309,
      660
    ],
    "inspect_ms": 20000,
    "hangup_on_detection": false
  }
}
```

- `version=1` 是此协议版本，不是 RustPBX 或服务软件版本。
- `call_id/session_id` 用于呼叫和逻辑会话关联。
- `side` 只能为 caller 或 callee，表示该连接音频来源；`both` 会产生两条连接，不是单条 start 的 side 值。
- `direction` 为 inbound/outbound/internal，是业务方向，和 side 不同。
- `tenant_id/inbound_trunk_id/outbound_trunk_id` 来自 PBX 路由上下文。
- `sample_rate` 是该 leg 的解码输出采样率，两条 leg 可不同，连接内固定。不要无条件写成 16000。

服务要求首条 start 在 5 秒内到达。无需返回 start ACK；参数解析与资源准备完成后消费音频。省略 parameters 或 `{}` 使用服务默认值；显式 null、未知参数或无效值会被拒绝。

### 5.3 PBX 发送 Binary 音频

**单声道、有符号 16-bit PCM、little-endian**，不带 RTP/WAV 头，不是 base64，也不是 JSON 数组。消息长度不要求等于一个 RTP 包；样本字节总数应为偶数，单条消息最多 64 KiB。

支持输入采样率 8000、12000、16000、24000、32000、44100、48000 Hz。RustPBX 负责解码，服务使用 Rubato 在需要时重采样到 16 kHz，16 kHz 输入无需重采样。比如 8 kHz 音频 20 ms 是 160 个 i16 样本，即 320 字节；16 kHz 同时长是 640 字节。

不要把 RTP 的网络字节序与 PCM16LE 线格式混淆：网络收到编码音频，解码得到样本值，WebSocket 接口约定这些值以小端字节排列发送。

### 5.4 服务返回事件及其影响

返回事件均为独立 Text JSON。服务不回传 side，PBX 按连接补充。

**text：转录文本，不会标记语音信箱，也不会挂断。**

```json
{"type":"text","text":"Please record your message after the tone",
 "partial":false,"start_ms":0,"end_ms":3100,"lang":"en"}
```

PBX 向 RWI/CC 消费者转发：partial 使用 `transcript_segment`，final 使用 `transcript_final`，不会将同一个 final 同时发送两遍。不要假设普通代理呼叫的文本自动存成 CDR transcript；持久化质检证据应使用 calltrace。

**calltrace：写入通话轨迹，不改变电话的接通状态。**

```json
{"type":"calltrace","kind":"quality",
 "code":"quality.voicemail_detected","severity":"info",
 "message":"Voicemail detected from configured ASR phrase",
 "detail":{"method":"asr","language":"en",
   "phrase":"after the tone","matched_text":"AFTER THE TONE",
   "end_ms":3100}}
```

beep 也用同一个 code，以 detail.method 区分：

```json
{"type":"calltrace","kind":"quality",
 "code":"quality.voicemail_detected","severity":"info",
 "message":"Voicemail detected from configured beep",
 "detail":{"method":"beep","detection":{
   "frequency_hz":1309,"start_ms":540,"end_ms":700,
   "tone_ms":160,"energy_ratio":0.99}}}
```

code 表示判断，method 表示检测方法。当前只实现 voicemail 检测，不把没有命中解释为真人或 AI。自定义服务可沿用 quality 类别、定义新 code 和 detail，不需新增 CDR 顶层列。PBX 为轨迹补充 side 和通话相对时间。

**hangup：明确要求 PBX 结束电话。**

```json
{"type":"hangup","reason":"quality.voicemail_detected",
 "detail":{"method":"asr","language":"en","end_ms":3100}}
```

PBX 没有额外的 allow_hangup 开关；只标记模式由服务不发送 hangup 保证。本服务在 `hangup_on_detection=true` 时先发检测 trace，再发 hangup，随后结束质检并 Close；false 时持续报告独立检测直到 inspect_ms。PBX 最终 End trace 保存 `detail.source="realtime_quality"`、reason 和证据；不要只用通用 autohangup 枚举判断来源。

**error：终止该质检连接，保持电话继续。**

```json
{"type":"error","message":"upstream recognizer unavailable"}
```

这是 PBX 支持的接口事件；当前示例服务主要通过日志和正常关闭处理连接错误，并不保证每种错误都会发送 error。

### 5.5 心跳、超时和正常结束

服务每 10 秒发送 WebSocket Ping，PBX 自动 Pong；PBX 默认在连续 15 秒未收到服务消息时关闭质检连接，音频持续上传不能替代服务心跳。连接建立超时为 3 秒。服务观察期由 inspect_ms 限制，关闭握手最多再等 1 秒。

双方都可发 WebSocket Close，服务没有更多音频需求时可以主动关闭，不需要发“最终判断”。Close、断网、检测超时或 idle timeout 只结束质检，**不等于挂断电话**。只有显式 hangup 才要求结束电话。

PBX 先处理顺序到达的 trace/hangup，再处理 Close；控制队列使用 try_send，满时记录丢弃日志，不提供业务 ACK 或重试投递保证。此设计优先保持 PBX 呼叫处理不被质检服务阻塞。

## 6. 详细配置参考

### 6.1 进程配置：资源与模型

| 字段 | 默认 / 范围 | 含义 |
| --- | --- | --- |
| `listen` | `127.0.0.1:8095` | 监听地址；本文示例显式使用 8096 |
| `token` | 不设置 | 设置后要求 Authorization: Bearer；不设置则不校验该认证 |
| `models` | 空 | 按 en / id 命名的模型表，仅加载所需语言 |
| `models.<lang>.encoder` | 必填路径 | 对应流式模型 encoder 文件 |
| `models.<lang>.decoder` | 必填路径 | 同套 decoder 文件 |
| `models.<lang>.joiner` | 必填路径 | 同套 joiner 文件 |
| `models.<lang>.tokens` | 必填路径 | 同套词表 tokens.txt |
| `models.<lang>.num_threads` | 1；1–32 | 该 recognizer 的内部计算线程配置，不是连接数 |
| `models.<lang>.max_batch_size` | 4；1–64 | 同语言一次批量 decode 的 stream 上限 |
| `models.<lang>.max_wait_ms` | 5；0–100 ms | 批处理调度等待配置；不是端到端响应时限 |

示例每语言 num_threads=2 可作为 8 核机器的起始值。多个语言 worker 和模型内部线程共享 CPU；不能据此承诺并发容量。路径建议用绝对路径，目录和文件应可读。只加载模型不会自动启用每连接 ASR。

### 6.2 每连接参数：ASR 与通用控制

| 选项 | 默认 / 范围 | 用途 |
| --- | --- | --- |
| asr_enabled | false | 是否将音频送入 ASR worker |
| asr_languages | ["id"]；en / id，无重复 | 启用 ASR 时需加载所选模型，并给每种语言配置规则 |
| keywords | []，最多 256 条 | 每条 language + phrase，或 language + sequence + max_gap_words |
| stable_updates | 2；1–10 | 同一规则连续多少次解码更新满足后确认；partial 也计数 |
| inspect_ms | 15000；20–300000 | 连接观察时间；示例 20000；到期关闭质检，不结束电话 |
| hangup_on_detection | false | ASR 和 beep 共用；true 时先 trace，再 hangup，随后 Close |

#### 短句与顺序规则

phrase 匹配连续单词；忽略大小写、标点和连续空白，但要求词边界。sequence 有 2–8 段按顺序出现的短句，max_gap_words 为每两段之间允许的词数（0–100）；0 表示紧邻。不能同时写 phrase 和 sequence，也不跨 ASR 语句累积锚点。

例如 ["please record", "after the tone"] + max_gap_words=4 可匹配“please record your message after the tone”。ASR 错字应通过业务词组选择/多个规则处理；不要使用过泛的单词，否则普通对话也可能命中。stable_updates 不是准确率指标，也不是等待毫秒数。

### 6.3 每连接参数：Beep

| 参数 | 默认 / 范围 | 说明 |
| --- | --- | --- |
| beep_enabled | true | 可与 ASR 同时开启，或只使用 beep |
| frequencies_hz | [1309, 660]；最多16项 | 目标频率；大于0、小于8000，容差边界也必须有效 |
| tolerance_hz | 20；0–100 | 检测频率容差，单位 Hz |
| min_tone_ms | 160；20–5000 | 至少连续达到的 tone 时长 |
| max_gap_ms | 40；0–500 | 允许 tone 中断的短间隙；0 不容许间隙 |
| min_dbfs | -35；-90–0 | 最低能量门限 |
| min_energy_ratio | 0.8；0.5–1.0 | 能量集中程度门限；更低更容易命中，也可能误报 |

#### 重复检测

hangup_on_detection=false 时，每段独立 beep、每种语言每个语句的首次确认命中都会上报。不限制连接总次数，但同一语句不断修正的 partial 不重复报告；持续的一段 tone 只报告一次。true 时仅发第一次检测的 trace 和 hangup，然后结束质检。

未检测到不等于真人：服务可以不发送任何结果，到观察截止时正常 Close。两种检测都关闭也正常结束连接。错误参数、未加载模型或服务不可达不应影响正常电话继续。

#### 缓冲、带宽与 CPU

RustPBX 解码 RTP 音频后发送 PCM16；服务按需重采样到 16 kHz。ASR 使用 100 ms 块，每语言每连接音频环形缓冲容纳 5 块；满了丢新块，不阻塞 PBX。beep 使用 20 ms 窗口。丢音频后 ASR 相对时间不再是原始录音的精确位置。

单声道 PCM16 在 8 kHz 时约 16 KB/s，在 16 kHz 时约 32 KB/s，尚未计入 WebSocket/TCP 开销；both 会传两路。num_threads 是模型内部线程数；不是每条连接的线程数。没有执行容量压测。



### 6.4 请求校验与旧参数迁移

- parameters 使用 serde 默认值和 validator 校验；基本数值范围即使 detector 关闭也要有效。未知字段拒绝。
- 每种启用的 ASR 语言必须已加载模型，并至少有一条对应语言的关键词规则；不存在程序硬编码关键词清单。
- `phrase` 或 sequence 中每段短句最长 512 字符，规范化后不能是空；sequence 与 phrase 二选一，sequence 必须配 max_gap_words。
- beep 开启时频率列表不能为空，`frequency ± tolerance` 必须严格位于 0–8000 Hz 之间。
- 只有一个 detector 开启也可工作；两者都关闭时正常结束质检连接。
- 旧 `hangup_on_asr` / `hangup_on_beep` 合并为 `hangup_on_detection`；删除 `text_language` / `report_each_detection`。ASR 开启后所选语言自动返回文本；独立检测不设整条连接的次数限制。
- PBX 不再使用 auto_start / allow_hangup / text_enabled；质检由 enabled 开启并在接听后启动。

## 7. Schema 变化与 PostgreSQL 迁移

以下区分 SQL 结构变化和已有 JSON 字段内容变化。

| 范围 | 改动 |
| --- | --- |
| `rustpbx_sip_trunks` | 新增 `register_username VARCHAR(160) NULL`；auth_username 原来已有；旧记录 NULL 时沿用旧鉴权用户名作为 REGISTER 用户名 |
| `rustpbx_tenants` | 新增 `quality_config JSON NULL`，保存策略选择 |
| `wholesale_routing_profile_items` | 新增 `quality_config JSON NULL`，保存条目覆盖选择 |
| `wholesale_quality_policies` | 新表：id 自增主键、name 唯一、config JSON |
| 选择数据转换 | 删除旧 disabled 字段；disabled=true 转为 policy_id=0 |
| CDR / 计费 | 没有新增质检列；检测和服务挂断继续保存在 metadata.trace，计费表结构不变 |
| PAI 配置 | 新开关使用现有 trunk metadata.sbc.send_p_asserted_identity，没有新增 SQL 列 |

REGISTER 字段已在此前 Wholesale 0.5.2.2 的升级范围内；0.5.2.2 → 0.5.2.3 新增的是质检表、两列和选择数据迁移。PAI 开关在随后 core main 中，不能仅凭 Wholesale 0.5.2.3 标签假定已经包含。

正常启动会执行内置迁移：核心记录在 seaql_migrations，Wholesale 记录在 wholesale_migrations。如果部署需先手动执行 SQL，Release 包内提供 `deploy/postgresql-quality-upgrade.sql`，与上述四项相关迁移等价：

```bash
psql -X -v ON_ERROR_STOP=1 "$DATABASE_URL" \
  -f deploy/postgresql-quality-upgrade.sql
```

它对已存在的业务库运行，使用连接的 search_path；事务内执行，可以重复运行。执行前备份，不要与应用启动迁移并发；不会批量回填 REGISTER 用户名，也不创建 policy id=0。脚本不伪造迁移历史，应用随后可重新检查并登记这四项幂等迁移。已在 PostgreSQL 临时表验证首次执行、旧 JSON 转换和重复执行。

旧 TOML `addons.wholesale.quality` 不能自动转为数据库策略：升级需要移除旧段，并改用本手册的 `[proxy.quality]` 或在 Wholesale 创建具体策略。数据转换只处理旧 disabled 选择，不会猜测或重建业务规则。

## 8. 查询结果与验收

控制台截图见 PDF。机器查询使用核心 CDR 的 `metadata.trace`；Wholesale 费用使用 call_id 关联 wholesale_cdrs。识别语音信箱不会免单，短通话仍按原最低计费时长/步长计费。

### 8.1 API 查询

需要具备 CDR read 权限的控制台 API token，它与服务 QUALITY_TOKEN 不同：

```bash
PBX_URL=http://127.0.0.1:9091
CDR_ID=2931  # replace with an existing call record ID
PBX_API_TOKEN='YOUR_CONSOLE_API_TOKEN'
curl -fsS -H "Authorization: Bearer $PBX_API_TOKEN" \
  "$PBX_URL/api/call-records/$CDR_ID/cdr-json" -o cdr.json
jq '[.metadata.trace[]? |
  select(.code == "quality.voicemail_detected")]' cdr.json
jq '[.metadata.trace[]? | select(.kind == "end" and
  .detail.source == "realtime_quality")]' cdr.json
```

默认只标记呼叫应有检测记录，没有服务来源 End；挂断模式则应有 source=realtime_quality 和 reason=quality.voicemail_detected。外层导出字段是 callId/hangupReason，不要把 HTML 详情页当作 JSON 接口。

### 8.2 PostgreSQL 查询服务挂断

```sql
SELECT c.id, c.call_id, e->'detail'->>'reason' AS reason
FROM rustpbx_call_records c
CROSS JOIN LATERAL jsonb_array_elements(
  COALESCE(c.metadata::jsonb->'trace', '[]'::jsonb)
) AS e
WHERE c.id BETWEEN 2926 AND 2931
  AND e->>'kind' = 'end'
  AND e->'detail'->>'source' = 'realtime_quality';
```

例子的 ID 范围来自本机测试，生产需要按时间/租户/ID 限定候选范围，再展开 JSON。SQL 不适用于直接复制到 SQLite/MySQL；跨数据库可使用同一个 CDR JSON API。

### 8.3 检查顺序

1. 服务 /health 返回 ok，日志确认 en 和 id 模型加载。
2. 全局测试时，Wholesale 租户和匹配条目的策略都留空；主配置 enabled=true，parameters 同时开启双语言和 beep。
3. 呼叫实际经过入站 trunk → 租户 → 路由条目 → 出站 trunk，接听后由被叫播放提示音。
4. 检查 quality.voicemail_detected 的 method/language，默认模式应看到正常 caller/callee 结束，不能是 realtime_quality 请求挂断。
5. 核对录音、SIPFlow、CDR 和 Wholesale 费用/余额；再测试具体策略覆盖一般配置。

本地 Wholesale 应用已实测两种入口：策略模式 CDR 2931；纯全局配置模式 CDR 2932（租户与条目均为 NULL）。全局模式记录英语、印尼语、beep 三次检测，质检在 20 秒结束后电话继续至主叫在 24 秒结束；CDR、录音和 Wholesale 计费/余额核验通过。这里验证的是本地 Wholesale 程序，不是容器镜像验收；未进行容量压测。
