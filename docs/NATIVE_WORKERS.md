# 原生文件验证与进程探针

开发入口是 Rust `xtask`。它用于 M0 的固定文件与 stdio 协议验证，不启动桌面、音频播放或麦克风；具体后端和分发门槛见[迁移路线](NATIVE_MIGRATION.md)及[验证证据](NATIVE_M0_EVIDENCE.md)。程序只按显式参数启动工作进程，验证者应使用本项目构建的程序和已核对的模型配置。

## 文件校验

在仓库根目录执行：

```powershell
cargo xtask native-verify 'D:/NativeValidation/model.json' 'D:/NativeValidation/model'
```

第一个文件使用 `ai-ex-model-catalog::ModelManifest` 格式，记录固定版本、后端、能力、必要文件的大小与 SHA256、许可来源和运行库要求。第二个参数是清单内相对路径的根目录。校验不写入或下载文件。

退出 0 表示所声明文件均符合清单；缺失、摘要错误、链接、Windows 重解析点或不合法清单均退出 1。JSON 中的 `ready` 仅描述声明文件，不证明模型已推理、依赖版本兼容或分发材料完整；这些范围在报告中单独标为未评估。

## stdio 探针

```powershell
cargo xtask native-probe 'D:/NativeValidation/probe.json'
```

探针输入是 UTF-8 JSON，最多 1 MiB。它先校验全部模型，再启动一个自有工作进程，等待就绪，按顺序发送请求，最后退出并回收进程。示例中的路径、模型与空间标识需要替换为实际验证输入：

```json
{
  "schema_version": 1,
  "executable": "D:/NativeValidation/bge-worker.exe",
  "arguments": ["--config", "D:/NativeValidation/worker.json"],
  "environment": {},
  "job_directory": "D:/NativeValidation/jobs",
  "ready_timeout_ms": 120000,
  "models": [{
    "manifest": "D:/NativeValidation/model.json",
    "directory": "D:/NativeValidation/model"
  }],
  "steps": [{
    "timeout_ms": 120000,
    "request": {
      "identity": {
        "session_id": "00000000-0000-0000-0000-000000000001",
        "request_id": "00000000-0000-0000-0000-000000000002",
        "generation": 1
      },
      "model_id": "bge-small-zh-v1.5",
      "task": {
        "type": "embed",
        "text": "你好，今天一起学习。",
        "space": "replace-with-verified-model-space"
      }
    }
  }]
}
```

所有路径必须为绝对路径，音频引用仍是作业目录内的受限相对 WAV 路径。作业目录必须预先存在；模型清单须包含每个请求的模型及对应能力。每次最多 8 个模型、16 个步骤，启动和请求时限为 1–600000 ms，请求 ID 不能重复。未知字段会报错，避免拼写错误静默改变验证范围。

需验证取消时，在最后一个步骤加入 `"cancel_after_ms": 500`，且小于该步骤的时限。监督器先撤销结果交付，最多等待一秒协作确认，再终止其持有的进程；取消请求不重放。定时取消只验证控制路径，不能证明取消发生在原生计算中；后者还需对应请求的后端开始标记及 CPU 时间证据。

报告包含文件结果、启动与请求耗时、输出、取消结果、进程是否回收和有界 stderr 尾部。音频输出会检查真实 RIFF 长度、标准 16 字节格式块、单声道 PCM16、声明采样率和样本长度，不能仅以 `.wav` 后缀验收。失败退出 1；结果中止后仍会执行进程回收。

这些入口不验证真实播放时钟、设备听感、服务接入或首次使用向导，也不将文件清单中的许可声明视为完整分发审核。保留本地探针输入和报告，向公开仓库提交前核对其中的路径、文本和日志是否含私人内容。
