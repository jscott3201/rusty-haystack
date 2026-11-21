use serde::Serialize;
use std::{io, time::Duration};
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub max_json_depth: usize,
    pub max_response_bytes: usize,
    pub max_in_flight: usize,
    pub max_pending_responses: usize,
    pub request_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 65_536,
            max_json_depth: 32,
            max_response_bytes: 1024 * 1024,
            max_in_flight: 16,
            max_pending_responses: 16,
            request_timeout: Duration::from_secs(10),
        }
    }
}
impl Limits {
    pub(crate) fn validate(&self, profile: crate::OutputProfile) -> Result<(), McpError> {
        if !(1024..=1024 * 1024).contains(&self.max_frame_bytes)
            || !(4..=64).contains(&self.max_json_depth)
            || !(profile.min_response_bytes()..=16 * 1024 * 1024).contains(&self.max_response_bytes)
            || !(1..=128).contains(&self.max_in_flight)
            || !(1..=128).contains(&self.max_pending_responses)
            || self.request_timeout.is_zero()
            || self.request_timeout > Duration::from_secs(60)
        {
            return Err(McpError::Configuration);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    #[error("invalid MCP transport configuration")]
    Configuration,
    #[error("MCP input framing limit or framing contract failed")]
    Framing,
    #[error("MCP output capacity exhausted")]
    Capacity,
    #[error("MCP response exceeds the transport limit")]
    Output,
    #[error("MCP request or response delivery deadline exceeded")]
    Deadline,
    #[error("MCP response delivery cancelled after it began")]
    CancelledDelivery,
    #[error("MCP transport I/O failed")]
    Io,
    #[error("MCP protocol session failed")]
    Protocol,
    #[error("MCP owned task failed")]
    Task,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub admitted: usize,
    pub released: usize,
    pub peak_in_flight: usize,
    pub delivered: usize,
    pub suppressed: usize,
}

/// Bounded serializer used before queueing, including JSON escaping and both
/// structured and text content. The vector never grows beyond the selected cap.
pub(crate) fn encode<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, McpError> {
    struct Bounded {
        bytes: Vec<u8>,
        max: usize,
    }
    impl io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("bounded JSON output"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        max,
    };
    serde_json::to_writer(&mut output, value).map_err(|_| McpError::Output)?;
    Ok(output.bytes)
}

pub(crate) fn depth_ok(bytes: &[u8], max: usize) -> bool {
    let (mut depth, mut string, mut escape) = (0usize, false, false);
    for byte in bytes {
        if string {
            if escape {
                escape = false;
            } else if *byte == b'\\' {
                escape = true;
            } else if *byte == b'"' {
                string = false;
            }
        } else {
            match byte {
                b'"' => string = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > max {
                        return false;
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    true
}
/// Reject duplicate members before typed decoders can choose an interpretation.
#[cfg(feature = "http")]
pub(crate) fn strict_json(bytes: &[u8], depth: usize) -> Result<serde_json::Value, ()> {
    use serde::{
        Deserialize,
        de::{self, MapAccess, SeqAccess, Visitor},
    };
    if !depth_ok(bytes, depth) {
        return Err(());
    }
    struct Strict(serde_json::Value);
    impl<'de> Deserialize<'de> for Strict {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Strict;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON without duplicate members")
                }
                fn visit_bool<E: de::Error>(self, v: bool) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_i64<E: de::Error>(self, v: i64) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_u64<E: de::Error>(self, v: u64) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_f64<E: de::Error>(self, v: f64) -> Result<Strict, E> {
                    serde_json::Number::from_f64(v)
                        .map(|v| Strict(v.into()))
                        .ok_or_else(|| E::custom("finite number required"))
                }
                fn visit_str<E: de::Error>(self, v: &str) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_string<E: de::Error>(self, v: String) -> Result<Strict, E> {
                    Ok(Strict(v.into()))
                }
                fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
                    Ok(Strict(serde_json::Value::Null))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
                    let mut v = Vec::new();
                    while let Some(Strict(item)) = a.next_element()? {
                        v.push(item);
                    }
                    Ok(Strict(v.into()))
                }
                fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
                    let mut v = serde_json::Map::new();
                    while let Some((k, Strict(item))) = a.next_entry::<String, Strict>()? {
                        if v.insert(k, item).is_some() {
                            return Err(de::Error::custom("duplicate member"));
                        }
                    }
                    Ok(Strict(v.into()))
                }
            }
            d.deserialize_any(V)
        }
    }
    let mut d = serde_json::Deserializer::from_slice(bytes);
    let Strict(value) = Strict::deserialize(&mut d).map_err(|_| ())?;
    d.end().map_err(|_| ())?;
    Ok(value)
}
