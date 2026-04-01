use crate::config::Config;
use crate::detect;
use crate::format::{self, DecodeOutput, FormatRegistry};
use crate::transform;

pub fn decode_with_transform(
    registry: &FormatRegistry,
    config: &Config,
    file_path: &str,
    data: &[u8],
) -> DecodeOutput {
    let mut output = format::decode_file(
        registry,
        config.forced_format.as_deref(),
        file_path,
        data,
        &config.format_params,
    )
    .unwrap_or_else(|e| {
        eprintln!("  warning: {e}");
        format::detect_and_decompress(data)
    });

    if let Some(ref argv) = config.transform_to_content {
        if let Ok(transformed) = transform::execute(argv, &output.data, &config.format_params, None)
        {
            let fmt = detect::detect(&transformed);
            output = DecodeOutput {
                data: transformed,
                format: fmt,
            };
        }
    }

    output
}
