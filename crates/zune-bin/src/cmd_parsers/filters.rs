/*
 * Copyright (c) 2023.
 *
 * This software is free software;
 *
 * You can redistribute it or modify it under terms of the MIT, Apache License or Zlib license
 */

use clap::ArgMatches;
use log::debug;
use zune_image::pipelines::Pipeline;
use zune_image::traits::IntoImage;
use zune_imageprocs::box_blur::BoxBlur;
use zune_imageprocs::convolve::Convolve;
use zune_imageprocs::gaussian_blur::GaussianBlur;
use zune_imageprocs::median::Median;
use zune_imageprocs::scharr::Scharr;
use zune_imageprocs::sobel::Sobel;
use zune_imageprocs::spatial::SpatialOps;
use zune_imageprocs::spatial_ops::SpatialOperations;
use zune_imageprocs::unsharpen::Unsharpen;
//use zune_opencl::ocl_sobel::OclSobel;

pub fn parse_options<T: IntoImage>(
    workflow: &mut Pipeline<T>, argument: &str, args: &ArgMatches
) -> Result<(), String> {
    if argument == "box-blur" {
        let radius = *args.get_one::<usize>(argument).unwrap();
        debug!("Added box blur filter with radius {}", radius);

        let box_blur = BoxBlur::new(radius);
        workflow.add_operation(Box::new(box_blur));
    } else if argument == "blur" {
        let sigma = *args.get_one::<f32>(argument).unwrap();
        debug!("Added gaussian blur filter with radius {}", sigma);

        let gaussian_blur = GaussianBlur::new(sigma);
        workflow.add_operation(Box::new(gaussian_blur));
    } else if argument == "unsharpen" {
        // parse first one as threshold
        let values: Vec<f32> = args.get_many::<f32>(argument).unwrap().copied().collect();
        let sigma_f32 = values[0];
        let threshold_u16 = values[1];

        debug!(
            "Added unsharpen filter with sigma={} and threshold={}",
            sigma_f32, threshold_u16
        );

        let unsharpen = Unsharpen::new(sigma_f32, threshold_u16 as u16, 0);
        workflow.add_operation(Box::new(unsharpen))
    } else if argument == "mean-blur" {
        let radius = *args.get_one::<usize>(argument).unwrap();
        debug!("Added mean blur filter with radius {}", radius);

        let mean_blur = SpatialOps::new(radius, SpatialOperations::Mean);
        workflow.add_operation(Box::new(mean_blur));
    } else if argument == "sobel" {
        debug!("Added sobel filter");
        workflow.add_operation(Box::new(Sobel::new()));
    } else if argument == "scharr" {
        debug!("Added scharr filter");
        workflow.add_operation(Box::new(Scharr::new()))
    } else if argument == "convolve" {
        debug!("Adding convolution filter");

        let values: Vec<f32> = args
            .get_many::<f32>(argument)
            .unwrap()
            .collect::<Vec<&f32>>()
            .iter()
            .map(|x| **x)
            .collect();

        workflow.add_operation(Box::new(Convolve::new(values, 1.0)))
    } else if argument == "median-blur" {
        let radius = *args.get_one::<usize>(argument).unwrap();

        let blur = Median::new(radius);
        debug!("Added median blur with  radius of {radius}");
        workflow.add_operation(Box::new(blur));
    }

    Ok(())
}
