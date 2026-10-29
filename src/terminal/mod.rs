pub mod font;
pub mod terminal_state;  // kept as stub — replaced by libghostty-vt
pub mod pty_session;
pub mod renderer;
pub mod registry;

pub use registry::TerminalRegistry;
