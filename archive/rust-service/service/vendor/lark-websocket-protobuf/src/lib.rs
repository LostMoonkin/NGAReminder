pub mod pbbp2 {
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Header {
        #[prost(string, tag = "1")]
        pub key: String,
        #[prost(string, tag = "2")]
        pub value: String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Frame {
        #[prost(uint64, tag = "1")]
        pub seq_id: u64,
        #[prost(uint64, tag = "2")]
        pub log_id: u64,
        #[prost(int32, tag = "3")]
        pub service: i32,
        #[prost(int32, tag = "4")]
        pub method: i32,
        #[prost(message, repeated, tag = "5")]
        pub headers: Vec<Header>,
        #[prost(string, optional, tag = "6")]
        pub payload_encoding: Option<String>,
        #[prost(string, optional, tag = "7")]
        pub payload_type: Option<String>,
        #[prost(bytes = "vec", optional, tag = "8")]
        pub payload: Option<Vec<u8>>,
        #[prost(string, optional, tag = "9")]
        pub log_id_new: Option<String>,
    }
}
