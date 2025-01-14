use core::fmt::Debug;
use std::fmt::Formatter;

pub enum GifDecoderErrors {
    /// File is not a gif
    NotAGif,
    /// A generic error
    Static(&'static str),
    /// To large dimensions for width or height
    TooLargeDimensions(&'static str, usize, usize)
}
impl Debug for GifDecoderErrors {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            GifDecoderErrors::NotAGif => {
                writeln!(f, "Not a gif, magic bytes didn't match")
            }
            GifDecoderErrors::Static(v) => {
                writeln!(f, "{}", v)
            }
            GifDecoderErrors::TooLargeDimensions(a, b, c) => {
                writeln!(
                    f,
                    "Too large dimensions for {a} expected less than {b} but found  {c}"
                )
            }
        }
    }
}

impl From<&'static str> for GifDecoderErrors {
    fn from(value: &'static str) -> Self {
        Self::Static(value)
    }
}
