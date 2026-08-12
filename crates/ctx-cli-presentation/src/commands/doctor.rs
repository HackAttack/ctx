use clap::Args;

use crate::output::JsonOutputFormat;

#[derive(Debug, Args, Clone)]
pub struct DoctorArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}
