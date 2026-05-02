pub const CHAT_PROTO: &str = "/speer/chat/1.0.0";
pub const CHAT_SERVICE_TYPE: &str = "_speer-chat._tcp";

pub const MAX_PEERS: usize = 16;
pub const MAX_NICK_LEN: usize = 32;
pub const MAX_TEXT_LEN: usize = 1024;
pub const MAX_RX_FILES: usize = 8;
pub const FILE_CHUNK_BYTES: usize = 384;
pub const HANDSHAKE_TIMEOUT_MS: i32 = 10_000;

pub const CHAT_TYPE_HELLO: u32 = 1;
pub const CHAT_TYPE_MSG: u32 = 2;
pub const CHAT_TYPE_BYE: u32 = 3;
pub const CHAT_TYPE_FILE_META: u32 = 4;
pub const CHAT_TYPE_FILE_CHUNK: u32 = 5;
pub const CHAT_TYPE_FILE_DONE: u32 = 6;
pub const CHAT_TYPE_FILE_ACCEPT: u32 = 7;
