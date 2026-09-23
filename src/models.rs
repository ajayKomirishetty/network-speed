#[derive(Debug, Clone)]
pub struct ThroughputSample {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub transfer_bytes: u64,
    pub bits_per_second: f64,
    pub retransmits: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct TestSummary {
    pub sender_bits_per_second: Option<f64>,
    pub receiver_bits_per_second: Option<f64>,
    pub sent_bytes: Option<u64>,
    pub received_bytes: Option<u64>,
    pub retransmits: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum TestEvent {
    Started,
    Throughput(ThroughputSample),
    Finished(TestSummary),
    Error(String),
    Cancelled,
}
