//! The assistant's own messages to the user (notices, errors, `Err` texts), in English. The UI
//! translates them: a `{name}` is filled in after translation, as `i18n::format` does. Messages
//! from a provider's API are passed on as they come.

pub const STOPPED: &str = "Stopped";
pub const DESKTOP_ONLY: &str = "The AI assistant needs the desktop app";
pub const CHOOSE_MODEL: &str = "Choose a model in Options ▸ AI.";
/// `{provider}`: the provider's label.
pub const ADD_KEY: &str = "Add your {provider} API key in Options ▸ AI.";
pub const SWITCHED: &str = "Switched to {model}: the conversation starts over.";
pub const INSTRUCTIONS_CHANGED: &str = "The custom instructions changed: the conversation starts over.";
pub const STOPPED_ROUNDS: &str = "Stopped after {rounds} rounds.";
pub const STOPPED_TOOL_ROUNDS: &str = "Stopped after {rounds} tool rounds; send a message to continue.";
/// `{error}`: what failed, as the provider or the connection said it.
pub const RETRYING: &str = "{error}. Trying again in {seconds} s ({attempt}/{attempts})";
pub const CHECK_KEY: &str = "{error}. Check the API key in Options ▸ AI.";
/// `{base}`: the base URL in Options ▸ AI.
pub const CANT_REACH: &str = "Can't reach {base}. Is the local model server (Ollama, LM Studio…) running?";
pub const NO_IMAGES: &str = "This model does not take images; the assistant goes on without them.";
pub const LENGTH_LIMIT: &str = "The reply reached the model's length limit.";
pub const CONTENT_FILTER: &str = "The provider's content filter stopped the reply.";
pub const MAX_TOKENS: &str = "The reply reached the output limit ({max_tokens} tokens); ask for a smaller step or to continue.";
pub const DECLINED: &str = "Claude declined this request.";
/// `{reason}`: the refusal's category and explanation.
pub const DECLINED_WHY: &str = "Claude declined this request ({reason}).";
pub const CONNECTED: &str = "Connected: {count} models";
pub const KEYCHAIN_UNAVAILABLE: &str = "The system keychain is not available: {error}";
pub const KEY_NOT_SAVED: &str = "Could not save the key in the system keychain: {error}";
pub const KEY_NOT_REMOVED: &str = "Could not remove the key from the system keychain: {error}";

/// Every message above, for the UI's translation catalog and its check.
pub const ALL: &[&str] = &[
    STOPPED,
    DESKTOP_ONLY,
    CHOOSE_MODEL,
    ADD_KEY,
    SWITCHED,
    INSTRUCTIONS_CHANGED,
    STOPPED_ROUNDS,
    STOPPED_TOOL_ROUNDS,
    RETRYING,
    CHECK_KEY,
    CANT_REACH,
    NO_IMAGES,
    LENGTH_LIMIT,
    CONTENT_FILTER,
    MAX_TOKENS,
    DECLINED,
    DECLINED_WHY,
    CONNECTED,
    KEYCHAIN_UNAVAILABLE,
    KEY_NOT_SAVED,
    KEY_NOT_REMOVED,
];
