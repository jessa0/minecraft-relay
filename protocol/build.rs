fn main() {
    prost_build::Config::default()
        .bytes(&["."])
        .compile_protos(&["src/minecraft-relay.proto"], &["src/"])
        .unwrap();
}
