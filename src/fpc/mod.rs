//! The FPC runner and the message pipeline (spec section 4): invoke the
//! compiler with a strict profile, parse its output into structured messages,
//! and normalize those into the single diagnostic model — expanding points to
//! spans and suppressing cascades.

pub mod normalize;
pub mod parser;
pub mod profile;
pub mod runner;

pub use normalize::{normalize, NormalizeOptions, NormalizedOutput};
pub use parser::{parse_output, RawFpcMessage, RawKind};
pub use profile::{build_profile, FpcMode, FpcProfile};
pub use runner::{run_fpc, FpcOutput};
