//! unskilled's own UI toolkit: springs, a GPU scene renderer and the dot-matrix font.
//! No UI framework underneath — just `wgpu`.

pub mod anim;
pub mod color;
pub mod geom;
pub mod render;
pub mod scene;
pub mod text;

pub use anim::{Spring, SpringConfig};
pub use color::Color;
pub use geom::Rect;
pub use render::Renderer;
pub use scene::Scene;
pub use text::Weight;
