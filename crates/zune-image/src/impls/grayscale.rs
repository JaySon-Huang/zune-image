use log::warn;
use zune_core::colorspace::ColorSpace;
use zune_imageprocs::grayscale::rgb_to_grayscale;

use crate::errors::ImgOperationsErrors;
use crate::image::{Image, ImageChannels};
use crate::traits::OperationsTrait;

/// Convert RGB data to grayscale
///
/// This will convert any image that contains three
/// RGB channels(including RGB, RGBA,RGBX) into grayscale
///
/// Formula for RGB to grayscale conversion is given by
///
/// ```text
///Grayscale = 0.299R + 0.587G + 0.114B
/// ```
/// but it's implemented using fixed point integer mathematics and simd kernels
/// where applicable (see zune-imageprocs/grayscale)
pub struct RgbToGrayScale
{
    preserve_alpha: bool,
}

impl RgbToGrayScale
{
    #[allow(clippy::new_without_default)]
    pub fn new() -> RgbToGrayScale
    {
        RgbToGrayScale {
            preserve_alpha: false,
        }
    }
    pub fn preserve_alpha(mut self, yes: bool) -> RgbToGrayScale
    {
        self.preserve_alpha = yes;
        self
    }
}
impl OperationsTrait for RgbToGrayScale
{
    fn get_name(&self) -> &'static str
    {
        "RGB to Grayscale"
    }

    fn _execute_simple(&self, image: &mut Image) -> Result<(), ImgOperationsErrors>
    {
        let im_colorspace = image.get_colorspace();

        if im_colorspace == ColorSpace::Luma
        {
            warn!("Image already in grayscale skipping this operation");
            return Ok(());
        }

        let (width, height) = image.get_dimensions();
        let size = width * height;

        let mut grayscale = vec![0; size];

        if let ImageChannels::ThreeChannels(rgb_data) = image.get_channel_ref()
        {
            rgb_to_grayscale((&rgb_data[0], &rgb_data[1], &rgb_data[2]), &mut grayscale);

            image.set_image_channel(ImageChannels::OneChannel(grayscale));
            image.set_colorspace(ColorSpace::Luma);
        }
        else if let ImageChannels::FourChannels(rgba_data) = image.get_channel_mut()
        {
            // discard alpha channel
            rgb_to_grayscale(
                (&rgba_data[0], &rgba_data[1], &rgba_data[2]),
                &mut grayscale,
            );

            if self.preserve_alpha
            {
                let alpha = std::mem::take(&mut rgba_data[4]);

                image.set_image_channel(ImageChannels::TwoChannels([grayscale, alpha]));
                image.set_colorspace(ColorSpace::LumaA);
            }
            else
            {
                image.set_image_channel(ImageChannels::OneChannel(grayscale));
                image.set_colorspace(ColorSpace::Luma);
            }
        }
        else
        {
            static ERR_MESSAGE: &str = "Expected layout of separated RGB(A) data wasn't found\
            ,perhaps you need to run `deinterleave` operation before calling RGB to grayscale";

            return Err(ImgOperationsErrors::InvalidChannelLayout(ERR_MESSAGE));
        }

        Ok(())
    }

    fn supported_colorspaces(&self) -> &'static [ColorSpace]
    {
        &[
            ColorSpace::RGBA,
            ColorSpace::RGB,
            ColorSpace::LumaA,
            ColorSpace::Luma,
            ColorSpace::RGBX,
        ]
    }
}
