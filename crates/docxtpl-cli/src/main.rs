//! docxtpl CLI：`docxtpl render <template> <context.json> <output>`。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use docxtpl_rs::{DocxTemplate, RenderOptions};

#[derive(Parser)]
#[command(
    name = "docxtpl",
    version,
    about = "docxtpl-rs 命令行（语义对齐 docxtpl 0.20.2）"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 用 JSON 上下文渲染一个 docx 模板。
    Render {
        /// 模板 .docx 路径。
        template: PathBuf,
        /// 上下文 JSON 文件路径。
        context: PathBuf,
        /// 输出 .docx 路径。
        output: PathBuf,
        /// 开启 autoescape（上游默认关闭，属显式增强）。
        #[arg(long)]
        autoescape: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Render {
            template,
            context,
            output,
            autoescape,
        } => {
            let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                let ctx_text = std::fs::read_to_string(&context)?;
                let ctx: serde_json::Value = serde_json::from_str(&ctx_text)?;
                let tpl = DocxTemplate::open(&template)?;
                let opts = RenderOptions::compat().with_autoescape(autoescape);
                let doc = tpl.render(&ctx, &opts)?;
                doc.save(&output)?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    eprintln!("已渲染: {}", output.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("渲染失败: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
