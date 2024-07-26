fn main() -> std::io::Result<()> {
    prost_build::Config::new()
    .out_dir("src/grpc/protos")
    .type_attribute("routeguide.Point", "#[derive(Hash)]")
    .compile_protos(&["src/protos/route_guide.proto"], &["src"])?;
    Ok(())
}
