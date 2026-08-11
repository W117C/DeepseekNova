use serde_json::Value;

/// State machine that scavenges complete JSON objects out of a streaming text
/// feed. Feed it chunks with [`ScavengeStateMachine::process_chunk`]; it
/// buffers only the characters inside the outermost `{...}` pair and returns
/// each fully-balanced, parseable JSON value as soon as it is closed.
pub struct ScavengeStateMachine {
    brace_count: i32,
    in_string: bool,
    escape_next: bool,
    buffer: String,
}

impl Default for ScavengeStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ScavengeStateMachine {
    /// Create an empty scavenger.
    pub fn new() -> Self {
        Self {
            brace_count: 0,
            in_string: false,
            escape_next: false,
            buffer: String::new(),
        }
    }

    /// Process a new chunk of streaming text. Returns parsed JSON object if a complete one was found.
    pub fn process_chunk(&mut self, chunk: &str) -> Option<Value> {
        for c in chunk.chars() {
            if self.escape_next {
                self.escape_next = false;
                self.buffer.push(c);
                continue;
            }

            match c {
                '\\' => {
                    self.escape_next = true;
                    if self.brace_count > 0 {
                        self.buffer.push(c);
                    }
                }
                '"' => {
                    self.in_string = !self.in_string;
                    if self.brace_count > 0 {
                        self.buffer.push(c);
                    }
                }
                '{' if !self.in_string => {
                    self.brace_count += 1;
                    self.buffer.push(c);
                }
                '}' if !self.in_string => {
                    if self.brace_count > 0 {
                        self.brace_count -= 1;
                        self.buffer.push(c);

                        if self.brace_count == 0 {
                            // Attempt to parse completed buffer
                            let parsed: Option<Value> = serde_json::from_str(&self.buffer).ok();
                            self.buffer.clear();
                            if parsed.is_some() {
                                return parsed;
                            }
                        }
                    }
                }
                _ => {
                    if self.brace_count > 0 {
                        self.buffer.push(c);
                    }
                }
            }
        }
        None
    }
}
