//! Prints the inputs and outputs of the downloaded ONNX model.
fn main() {
    let dylib = memorycastle::embed::download::ensure_runtime(true).unwrap();
    let model = memorycastle::embed::download::model_dir().join("model_quantized.onnx");
    let env = ort::init_from(&dylib).unwrap().build().unwrap();
    let s = ort::session::Session::builder(&env).unwrap().commit_from_file(&model).unwrap();
    for i in s.inputs() {
        println!("in  {} {:?}", i.name(), i.dtype());
    }
    for o in s.outputs() {
        println!("out {} {:?}", o.name(), o.dtype());
    }
}
