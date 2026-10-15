use serde::{Deserialize, Serialize};
use threex_storage::{ProtocolAction, WorkerCode, WorkerFailure, WorkerState};
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub enum Locale {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "zh-CN")]
    ZhCn,
}
impl Locale {
    pub fn key(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
            Self::ZhCn => "zh-CN",
        }
    }
    pub(crate) fn hint(value: &serde_json::Value) -> Self {
        match value.get("locale").and_then(|v| v.as_str()) {
            Some("ja") => Self::Ja,
            Some("zh-CN") => Self::ZhCn,
            _ => Self::En,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IpcRequest {
    Draw {
        document_id: String,
        camera: [f32; 16],
        #[serde(default)]
        locale: Locale,
    },
    Hello {
        #[serde(default)]
        locale: Locale,
    },
    Inspect {
        document_id: String,
        #[serde(default)]
        locale: Locale,
    },
    ContentHash {
        document_id: String,
        #[serde(default)]
        locale: Locale,
    },
    Session {
        document_id: String,
        #[serde(default)]
        locale: Locale,
    },
    Save {
        document_id: String,
        #[serde(default)]
        locale: Locale,
    },
    Edit {
        document_id: String,
        session_id: String,
        sequence: String,
        action: EditAction,
        #[serde(default)]
        locale: Locale,
    },
}
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditAction {
    Commit {
        expected_revision: String,
        #[serde(deserialize_with = "command_bytes")]
        commands: Vec<u8>,
    },
    Undo {
        expected_revision: String,
    },
    Redo {
        expected_revision: String,
    },
    EndSession {
        expected_revision: String,
    },
}
fn command_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    struct Bytes;
    impl<'de> serde::de::Visitor<'de> for Bytes {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("at most 262144 command bytes")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            if seq.size_hint().is_some_and(|n| n > 256 * 1024) {
                return Err(serde::de::Error::custom("command limit"));
            }
            let mut b = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(256 * 1024));
            while let Some(v) = seq.next_element::<u8>()? {
                if b.len() == 256 * 1024 {
                    return Err(serde::de::Error::custom("command limit"));
                }
                b.push(v);
            }
            Ok(b)
        }
    }
    d.deserialize_seq(Bytes)
}
pub(crate) fn number(value: &str) -> Option<u64> {
    if value.len() > 20 || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = value.parse::<u64>().ok()?;
    (n.to_string() == value).then_some(n)
}
impl EditAction {
    pub(crate) fn protocol(self, locale: Locale) -> Result<ProtocolAction, ErrorDto> {
        let parse = |s: &str| number(s).ok_or_else(|| ErrorDto::host("invalid_request", locale));
        Ok(match self {
            Self::Commit {
                expected_revision,
                commands,
            } => ProtocolAction::Commit {
                expected_revision: parse(&expected_revision)?,
                commands,
            },
            Self::Undo { expected_revision } => ProtocolAction::Undo {
                expected_revision: parse(&expected_revision)?,
            },
            Self::Redo { expected_revision } => ProtocolAction::Redo {
                expected_revision: parse(&expected_revision)?,
            },
            Self::EndSession { expected_revision } => ProtocolAction::EndSession {
                expected_revision: parse(&expected_revision)?,
            },
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryAdvice {
    SameRequest,
    Resynchronize,
    NewRequest,
    None,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorDto {
    pub code: String,
    pub message: String,
    pub observed_revision: Option<String>,
    pub retry: RetryAdvice,
}
impl ErrorDto {
    pub(crate) fn host(code: &str, locale: Locale) -> Self {
        let (en, ja, zh) = match code {
            "surface_stale" => (
                "Surface is detached or replaced",
                "描画面は切断または再接続されています",
                "绘图表面已断开或更换",
            ),
            "surface_capacity" => (
                "Surface limit reached",
                "描画面の上限に達しました",
                "绘图表面达到上限",
            ),
            "surface_size" => (
                "Unsupported surface size",
                "描画面のサイズに対応していません",
                "不支持的绘图表面尺寸",
            ),
            "surface_unavailable" => (
                "Surface is unavailable",
                "描画面を利用できません",
                "绘图表面不可用",
            ),
            "device_lost" => (
                "GPU device lost; reconnect the surface",
                "GPU接続が失われました。描画面を再接続してください",
                "GPU设备已丢失，请重新连接绘图表面",
            ),
            "gpu_out_of_memory" => (
                "GPU memory exhausted; reconnect the surface",
                "GPUメモリが不足しています。描画面を再接続してください",
                "GPU内存不足，请重新连接绘图表面",
            ),
            "forbidden" => (
                "Access denied",
                "アクセスが許可されていません",
                "访问被拒绝",
            ),
            "session_forbidden" => (
                "Session is not assigned to this window",
                "このウインドウに割り当てられたsessionではありません",
                "会话未分配给此窗口",
            ),
            "document_mismatch" => ("Wrong document", "文書IDが一致しません", "文档 ID 不匹配"),
            "stopped" => (
                "Document host has stopped",
                "文書ホストは停止しています",
                "文档服务已停止",
            ),
            "queue_full" => (
                "Too many pending requests",
                "処理待ちの要求が上限に達しました",
                "待处理请求已满",
            ),
            _ => ("Invalid request", "無効な要求です", "无效请求"),
        };
        Self {
            code: code.into(),
            message: match locale {
                Locale::En => en,
                Locale::Ja => ja,
                Locale::ZhCn => zh,
            }
            .into(),
            observed_revision: None,
            retry: if code == "queue_full" {
                RetryAdvice::SameRequest
            } else {
                RetryAdvice::None
            },
        }
    }
    pub(crate) fn worker(failure: WorkerFailure, locale: Locale) -> Self {
        use WorkerCode::*;
        let code = match failure.code {
            QueueFull => "queue_full",
            Stopped => "stopped",
            InvalidId => "invalid_id",
            CommandTooLarge => "command_too_large",
            RequestMismatch => "request_mismatch",
            Expired => "expired",
            RevisionConflict => "revision_conflict",
            InvalidCommand => "invalid_command",
            HistoryCapacity => "history_capacity",
            NoHistory => "no_history",
            JournalLimit => "journal_limit",
            StorageFailure => "storage_failure",
            DocumentMismatch => "document_mismatch",
            SequenceGap => "sequence_gap",
            SessionLimit => "session_limit",
            SessionClosed => "session_closed",
            SessionUnknown => "session_unknown",
            UnsupportedProtocol => "unsupported_protocol",
        };
        Self {
            code: code.into(),
            message: failure.code.message(locale.key()).into(),
            observed_revision: Some(failure.current_revision.to_string()),
            retry: match failure.code {
                QueueFull | StorageFailure => RetryAdvice::SameRequest,
                RevisionConflict | InvalidCommand | HistoryCapacity | NoHistory => {
                    RetryAdvice::NewRequest
                }
                Expired | SequenceGap | RequestMismatch | JournalLimit | SessionClosed
                | SessionUnknown => RetryAdvice::Resynchronize,
                _ => RetryAdvice::None,
            },
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct StateDto {
    pub document_id: String,
    pub revision: String,
    pub objects: usize,
    pub saved_revision: Option<String>,
    pub dirty: bool,
    pub discarded_journal_bytes: String,
    pub successful_sync_groups: String,
}
impl From<WorkerState> for StateDto {
    fn from(s: WorkerState) -> Self {
        Self {
            document_id: s.document_id.to_string(),
            revision: s.revision.to_string(),
            objects: s.objects,
            saved_revision: s.saved_revision.map(|v| v.to_string()),
            dirty: s.dirty,
            discarded_journal_bytes: s.discarded_journal_bytes.to_string(),
            successful_sync_groups: s.successful_sync_groups.to_string(),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcReply {
    Frame {
        status: &'static str,
        state: StateDto,
    },
    Hello {
        state: StateDto,
        session_id: Option<String>,
    },
    State {
        state: StateDto,
    },
    Edited {
        revision: String,
        replayed: bool,
        state: StateDto,
    },
    Saved {
        generation: String,
        state: StateDto,
    },
    Session {
        sequence: Option<String>,
        status: Option<&'static str>,
        state: StateDto,
    },
    ContentHash {
        hash: String,
        version: u32,
        state: StateDto,
    },
}
