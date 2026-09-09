//! Custom `log::Log` subscriber routing records to the serial TX channel.

struct SerialLogger;

impl log::Log for SerialLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            crate::serial::log_record(record);
        }
    }

    fn flush(&self) {}
}

static LOGGER: SerialLogger = SerialLogger;

/// Initializes the global logger subscriber.
pub fn init() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}
