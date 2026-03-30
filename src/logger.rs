use std::io::{self, Write};

use chrono::{SecondsFormat, Utc};
use log::{Level, LevelFilter, Log, Metadata, Record};

pub fn init(level: LevelFilter) {
    log::set_boxed_logger(Box::new(ConsoleLogger(level))).expect("logger already set");
    log::set_max_level(level);
}

struct ConsoleLogger(LevelFilter);

impl Log for ConsoleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.0
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let level = format_level(record.level());
            let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let line = record.line().unwrap_or(0);

            let _ = match record.level() {
                Level::Error => {
                    writeln!(
                        io::stderr(),
                        "[{ts} {level} {}:{line}] {}",
                        record.target(),
                        record.args()
                    )
                }
                _ => {
                    writeln!(
                        io::stdout(),
                        "[{ts} {level} {}:{line}] {}",
                        record.target(),
                        record.args()
                    )
                }
            };
        }
    }

    fn flush(&self) {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
    }
}

fn format_level(level: Level) -> &'static str {
    match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN ",
        Level::Info => "INFO ",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    }
}
