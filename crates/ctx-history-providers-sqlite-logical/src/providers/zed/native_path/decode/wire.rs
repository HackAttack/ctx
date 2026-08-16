use super::*;

pub(super) fn push_nonempty(parts: &mut Vec<String>, value: String) {
    if !value.trim().is_empty() {
        parts.push(value);
    }
}

pub(super) fn nonempty_owned(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[derive(Deserialize)]
pub(super) struct ZedThreadWire {
    #[serde(default)]
    pub(super) version: Option<String>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) updated_at: Option<String>,
    #[serde(default)]
    pub(super) messages: Option<ZedValidatedMessages>,
}

pub(super) struct ZedValidatedMessages {
    pub(super) count: usize,
}

impl<'de> Deserialize<'de> for ZedValidatedMessages {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(ZedValidatedMessagesVisitor)
    }
}

struct ZedValidatedMessagesVisitor;

impl<'de> Visitor<'de> for ZedValidatedMessagesVisitor {
    type Value = ZedValidatedMessages;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed message sequence")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut count = 0_usize;
        while sequence.next_element::<ZedMessageWire>()?.is_some() {
            count = count.saturating_add(1);
        }
        Ok(ZedValidatedMessages { count })
    }
}

pub(super) enum ZedMessageWire {
    User(ZedUserWire),
    Agent(ZedAgentWire),
    Compaction(Option<String>),
    Resume,
    Unknown(String),
}

impl<'de> Deserialize<'de> for ZedMessageWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ZedMessageVisitor)
    }
}

struct ZedMessageVisitor;

impl<'de> Visitor<'de> for ZedMessageVisitor {
    type Value = ZedMessageWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed externally tagged message")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value.to_owned())
        })
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value.to_owned())
        })
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value)
        })
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let kind = map
            .next_key::<String>()?
            .ok_or_else(|| serde::de::Error::custom("Zed message tag is empty"))?;
        let message = match kind.as_str() {
            "User" => ZedMessageWire::User(map.next_value()?),
            "Agent" => ZedMessageWire::Agent(map.next_value()?),
            "Compaction" => {
                let value: ZedCompactionWire = map.next_value()?;
                ZedMessageWire::Compaction(value.summary)
            }
            "Resume" => {
                map.next_value::<IgnoredAny>()?;
                ZedMessageWire::Resume
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
                ZedMessageWire::Unknown(kind)
            }
        };
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Zed message must contain exactly one external tag",
            ));
        }
        Ok(message)
    }
}

#[derive(Deserialize)]
pub(super) struct ZedUserWire {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) content: Vec<ZedContentWire>,
}

#[derive(Deserialize)]
pub(super) struct ZedAgentWire {
    #[serde(default)]
    pub(super) content: Vec<ZedContentWire>,
    #[serde(default, rename = "tool_results")]
    pub(super) tool_results: Value,
}

#[derive(Deserialize)]
struct ZedCompactionWire {
    #[serde(default, rename = "Summary")]
    summary: Option<String>,
}

pub(super) enum ZedContentWire {
    Text(String),
    Thinking(String),
    RedactedThinking,
    ToolUse(ZedToolUseWire),
    ToolResult(Value),
    Mention(Option<String>),
    Image,
    Unknown(String),
}

impl<'de> Deserialize<'de> for ZedContentWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ZedContentVisitor)
    }
}

struct ZedContentVisitor;

impl<'de> Visitor<'de> for ZedContentVisitor {
    type Value = ZedContentWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed externally tagged content value")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let kind = map
            .next_key::<String>()?
            .ok_or_else(|| serde::de::Error::custom("Zed content tag is empty"))?;
        let content = match kind.as_str() {
            "Text" => ZedContentWire::Text(map.next_value()?),
            "Thinking" => {
                let value: ZedThinkingWire = map.next_value()?;
                ZedContentWire::Thinking(value.text.unwrap_or_default())
            }
            "RedactedThinking" => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::RedactedThinking
            }
            "ToolUse" => ZedContentWire::ToolUse(map.next_value()?),
            "ToolResult" => ZedContentWire::ToolResult(map.next_value()?),
            "Mention" => {
                let value: ZedMentionWire = map.next_value()?;
                ZedContentWire::Mention(value.content)
            }
            "Image" => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::Image
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::Unknown(kind)
            }
        };
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Zed content must contain exactly one external tag",
            ));
        }
        Ok(content)
    }
}

#[derive(Deserialize)]
struct ZedThinkingWire {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct ZedMentionWire {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ZedToolUseWire {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) input: Option<Value>,
    #[serde(default)]
    pub(super) raw_input: Option<String>,
}
