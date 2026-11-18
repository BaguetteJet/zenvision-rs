use std::time::Duration;

use crate::canvas::Canvas;

pub const DEFAULT_FPS: u32 = 30;

pub trait Animation {
    fn draw(&mut self, canvas: &mut Canvas, dt: Duration);
}

// Register animations here. Each module must provide:
//   pub struct Options: clap::Args + Clone + Default
//   pub struct <Variant> with new(Options)
// The module name is also the CLI value, so keep it one word.
macro_rules! animations {
    ($($variant:ident => $module:ident),+ $(,)?) => {
        $(mod $module;)+

        // One subcommand per animation. Options become CLI flags.
        #[derive(Clone, Debug, PartialEq, clap::Subcommand)]
        pub enum AnimationKind {
            $($variant($module::Options)),+
        }

        impl AnimationKind {
            pub fn create(&self) -> Box<dyn Animation> {
                match self {
                    $(Self::$variant(options) => {
                        Box::new($module::$variant::new(options.clone()))
                    })+
                }
            }

            pub fn name(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => stringify!($module)),+
                }
            }
        }
    };
}

animations! {
    Starfield => starfield,
}

impl Default for AnimationKind {
    fn default() -> Self {
        Self::Starfield(Default::default())
    }
}
