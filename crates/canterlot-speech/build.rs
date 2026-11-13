//! С фичей `cuda`: transcribe-cpp-sys 0.4.2 в статической сборке не заносит
//! библиотеки CUDA в свой манифест компоновки (transcribe-link.json), поэтому
//! ggml-cuda остаётся без cudart, cublas и драйвера. Добавляем их сами, динамически:
//! на проде они стоят в /opt/cuda-12.9/lib и в ldconfig, драйвер — libcuda.so.1.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    println!("cargo:rerun-if-env-changed=CUDAToolkit_ROOT");
    if std::env::var_os("CARGO_FEATURE_CUDA").is_none() {
        return;
    }
    let root = std::env::var_os("CUDA_PATH")
        .or_else(|| std::env::var_os("CUDAToolkit_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/local/cuda"));
    // Заглушка libcuda.so из stubs — после настоящих библиотек: её SONAME libcuda.so.1.
    for dir in ["lib64", "lib", "lib64/stubs", "lib/stubs"] {
        let path = root.join(dir);
        if path.is_dir() {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
    }
    for lib in ["cudart", "cublas", "cublasLt", "cuda"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
