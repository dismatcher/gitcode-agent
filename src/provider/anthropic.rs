//! Anthropic Messages encoding and indexed streaming block assembly.

use super::{
    canonical::{Content, Event, Request, Role, Usage},
    sse::Record,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Number of trailing message blocks that receive an explicit cache breakpoint.
///
/// The Messages API allows at most four `cache_control` markers per request and
/// the operator prompt consumes one, so the conversation keeps three.
const MESSAGE_CACHE_BREAKPOINTS: usize = 3;

/// Marks the trailing conversation blocks so a follow-up turn reuses them.
///
/// Only the last blocks of the final messages are marked: earlier positions are
/// served by the longest previously cached prefix, and each extra marker is
/// charged against the four-marker request limit.
fn apply_message_cache_breakpoints(messages: &mut [Value]) {
    let mut remaining = MESSAGE_CACHE_BREAKPOINTS;
    for message in messages.iter_mut().rev() {
        if remaining == 0 {
            break;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in content.iter_mut().rev() {
            if remaining == 0 {
                break;
            }
            if let Some(object) = block.as_object_mut() {
                object.insert("cache_control".into(), json!({"type":"ephemeral"}));
                remaining -= 1;
            }
        }
    }
}

/// Encode the authoritative Messages wire format.
///
/// `cache` marks the operator prompt and trailing conversation blocks with
/// `cache_control`. The Messages API ignores a top-level `cache_control` field,
/// so the markers must live on content blocks to actually reuse a prompt.
pub fn encode(request: &Request, cache: bool) -> Result<Value, String> {
    let mut messages = Vec::new();
    for message in &request.messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => return Err("system messages belong in the operator prompt".into()),
        };
        let mut content = Vec::new();
        for block in &message.content {
            content.push(match block {
                Content::Text {text} => json!({"type":"text","text":text}),
                Content::Thinking {thinking,signature,..} => json!({"type":"thinking","thinking":thinking,"signature":signature}),
                Content::ToolUse {id,name,input} => json!({"type":"tool_use","id":id,"name":name,"input":input}),
                Content::ToolResult {tool_use_id,content,is_error} => json!({"type":"tool_result","tool_use_id":tool_use_id,"content":content,"is_error":is_error}),
                Content::Image {media_type,data} => json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}}),
                Content::Document {media_type,data} => json!({"type":"document","source":{"type":"base64","media_type":media_type,"data":data}}),
            });
        }
        messages.push(json!({"role":role,"content":content}));
    }
    let tools: Vec<_> = request
        .tools
        .iter()
        .map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.input_schema}))
        .collect();
    // The operator prompt is a stable prefix across every turn, so it is the
    // cheapest block to cache. Tools accompany it in the same prefix.
    let system = if cache {
        json!([{
            "type":"text",
            "text":request.system,
            "cache_control":{"type":"ephemeral"},
        }])
    } else {
        json!(request.system)
    };
    if cache {
        apply_message_cache_breakpoints(&mut messages);
    }
    Ok(
        json!({"model":request.model,"system":system,"messages":messages,"tools":tools,"max_tokens":request.max_tokens,"stream":true}),
    )
}

#[derive(Default)]
struct Block {
    id: String,
    name: String,
    input: String,
    initial: Option<Value>,
}

/// Decode the buffered argument fragments of one tool block.
///
/// Compliant providers stream a single JSON object. Some compatible relays emit
/// several complete objects for one block, which concatenates to `{..}{..}`.
/// Each complete value is returned so no call is silently dropped.
fn parse_arguments(input: &str, initial: Option<Value>) -> Result<Vec<Value>, String> {
    if input.trim().is_empty() {
        return Ok(vec![initial.unwrap_or_else(|| json!({}))]);
    }
    let mut values = Vec::new();
    let mut stream = serde_json::Deserializer::from_str(input).into_iter::<Value>();
    for value in &mut stream {
        values.push(value.map_err(|_| "invalid tool argument JSON")?);
        if values.len() > 64 {
            return Err("tool block contains too many argument objects".into());
        }
    }
    if values.is_empty() {
        return Err("invalid tool argument JSON".into());
    }
    Ok(values)
}

/// Decoder preserves argument fragments until a block closes successfully.
#[derive(Default)]
pub struct Stream {
    blocks: BTreeMap<u64, Block>,
    open_blocks: BTreeMap<u64, String>,
    thinking_blocks: BTreeMap<u64, (String, String)>,
    seen_blocks: BTreeSet<u64>,
    started: bool,
    message_delta_seen: bool,
    usage: Usage,
    reason: String,
    done: bool,
}

impl Stream {
    /// Translate one named SSE record into canonical incremental events.
    pub fn accept(&mut self, record: Record) -> Result<Vec<Event>, String> {
        if self.done {
            return Err("provider event after message_stop".into());
        }
        if record.event == "ping" {
            return Ok(vec![]);
        }
        let value: Value =
            serde_json::from_str(&record.data).map_err(|_| "invalid provider event JSON")?;
        let kind = value["type"].as_str().unwrap_or(&record.event);
        let mut output = Vec::new();
        if !self.started && !matches!(kind, "message_start" | "ping" | "error") {
            return Err("provider event before message_start".into());
        }
        match kind {
            "message_start" => {
                if self.started {
                    return Err("duplicate message_start".into());
                }
                self.started = true;
                self.update_usage(&value["message"]["usage"]);
            }
            "content_block_start" => {
                let index = value["index"].as_u64().ok_or("missing block index")?;
                let block = &value["content_block"];
                let block_type = block["type"].as_str().ok_or("missing block type")?;
                if self.message_delta_seen || !self.seen_blocks.insert(index) {
                    return Err("duplicate or late content block".into());
                }
                self.open_blocks.insert(index, block_type.to_owned());
                if block_type == "thinking" {
                    self.thinking_blocks.insert(
                        index,
                        (
                            block["thinking"].as_str().unwrap_or_default().to_owned(),
                            block["signature"].as_str().unwrap_or_default().to_owned(),
                        ),
                    );
                }
                if block["type"] == "tool_use" {
                    self.blocks.insert(
                        index,
                        Block {
                            id: block["id"].as_str().ok_or("missing tool id")?.into(),
                            name: block["name"].as_str().ok_or("missing tool name")?.into(),
                            input: String::new(),
                            initial: Some(block["input"].clone()),
                        },
                    );
                }
            }
            "content_block_delta" => {
                let index = value["index"].as_u64().ok_or("missing block index")?;
                let block_type = self
                    .open_blocks
                    .get(&index)
                    .ok_or("delta for closed or unknown block")?;
                let delta = &value["delta"];
                let expected = match delta["type"].as_str() {
                    Some("text_delta") => Some("text"),
                    Some("thinking_delta" | "signature_delta") => Some("thinking"),
                    Some("input_json_delta") => Some("tool_use"),
                    _ => None,
                };
                if expected.is_some_and(|expected| expected != block_type) {
                    return Err("delta does not match content block type".into());
                }
                match delta["type"].as_str() {
                    Some("text_delta") => output.push(Event::TextDelta {
                        text: delta["text"].as_str().ok_or("missing text delta")?.into(),
                    }),
                    Some("thinking_delta") => {
                        let text = delta["thinking"].as_str().ok_or("missing thinking delta")?;
                        self.thinking_blocks
                            .get_mut(&index)
                            .ok_or("missing thinking block")?
                            .0
                            .push_str(text);
                        output.push(Event::ThinkingDelta { text: text.into() });
                    }
                    Some("input_json_delta") => {
                        let index = value["index"].as_u64().ok_or("missing block index")?;
                        let block = self
                            .blocks
                            .get_mut(&index)
                            .ok_or("arguments for unknown tool block")?;
                        block.input.push_str(
                            delta["partial_json"]
                                .as_str()
                                .ok_or("missing argument fragment")?,
                        );
                        if block.input.len() > 8 * 1024 * 1024 {
                            return Err("tool input exceeds limit".into());
                        }
                    }
                    Some("signature_delta") => {
                        let signature = delta["signature"]
                            .as_str()
                            .ok_or("missing signature delta")?;
                        self.thinking_blocks
                            .get_mut(&index)
                            .ok_or("missing thinking block")?
                            .1
                            .push_str(signature);
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = value["index"].as_u64().ok_or("missing block index")?;
                self.open_blocks
                    .remove(&index)
                    .ok_or("stop for closed or unknown block")?;
                if let Some((thinking, signature)) = self.thinking_blocks.remove(&index) {
                    output.push(Event::ThinkingComplete {
                        thinking,
                        signature: (!signature.is_empty()).then_some(signature),
                    });
                }
                if let Some(block) = self.blocks.remove(&index) {
                    let arguments = parse_arguments(&block.input, block.initial)?;
                    let multiple = arguments.len() > 1;
                    for (position, input) in arguments.into_iter().enumerate() {
                        if !input.is_object() {
                            return Err("tool arguments must be a JSON object".into());
                        }
                        output.push(Event::ToolCall {
                            // Some relays pack several calls into one block without
                            // distinct ids; keep every result addressable.
                            id: if multiple {
                                format!("{}-{}", block.id, position + 1)
                            } else {
                                block.id.clone()
                            },
                            name: block.name.clone(),
                            input,
                        });
                    }
                }
            }
            "message_delta" => {
                if !self.open_blocks.is_empty() {
                    return Err("message_delta before content blocks closed".into());
                }
                self.message_delta_seen = true;
                self.update_usage(&value["usage"]);
                self.reason = value["delta"]["stop_reason"]
                    .as_str()
                    .unwrap_or("unknown")
                    .into();
                output.push(Event::Usage {
                    usage: self.usage.clone(),
                });
            }
            "message_stop" => {
                if !self.message_delta_seen
                    || !self.open_blocks.is_empty()
                    || !self.blocks.is_empty()
                {
                    return Err("message stopped with incomplete tool arguments".into());
                }
                self.done = true;
                output.push(Event::Done {
                    reason: self.reason.clone(),
                });
            }
            "error" => return Err("provider returned a streaming error".into()),
            _ => {}
        }
        Ok(output)
    }

    /// Require an explicit message_stop before a turn can execute tools.
    pub fn finish(&self) -> Result<(), String> {
        if self.done && self.blocks.is_empty() {
            Ok(())
        } else {
            Err("provider stream ended before message_stop".into())
        }
    }

    fn update_usage(&mut self, value: &Value) {
        for (field, target) in [
            ("input_tokens", &mut self.usage.input_tokens),
            ("output_tokens", &mut self.usage.output_tokens),
            ("cache_read_input_tokens", &mut self.usage.cache_read_tokens),
            (
                "cache_creation_input_tokens",
                &mut self.usage.cache_write_tokens,
            ),
        ] {
            if let Some(n) = value[field].as_u64() {
                *target = n;
            }
        }
    }
}
