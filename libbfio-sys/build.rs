extern crate bindgen;

use failure::{bail, Error};
use std::env;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Build the lib.
/// This function will also add the needed folder to the `link-search` path.
/// Return the "include" folder for the library (to be used by bindgen).
#[cfg(not(target_os = "windows"))]
fn build_lib(lib_path: PathBuf, shared: bool) -> PathBuf {
    let target = lib_path.join("dist");

    println!("building with prefix={}", target.display());

    Command::new("sh")
        .arg("synclibs.sh")
        .current_dir(&lib_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("synclibs failed");

    Command::new("sh")
        .arg("autogen.sh")
        .current_dir(&lib_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("autogen failed");

    let mut configure_cmd = Command::new("sh");

    configure_cmd
        .arg("configure")
        .arg(format!("--prefix={}", target.display()))
        .current_dir(&lib_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit());

    if !shared {
        configure_cmd.arg("--enable-shared=no");
    }

    configure_cmd.status().expect("configure failed");

    Command::new("make")
        .current_dir(&lib_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("make failed");

    Command::new("make")
        .arg("install")
        .current_dir(&lib_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("make install failed");

    assert!(
        target.join("lib").exists(),
        "Expected {} to exist",
        target.join("lib").display()
    );

    println!(
        "cargo:rustc-link-search=native={}",
        target.join("lib").canonicalize().unwrap().to_string_lossy()
    );

    target.join("include")
}

#[cfg(target_os = "windows")]
fn build_lib(lib_path: PathBuf, shared: bool) -> PathBuf {
    let python_exec =
        env::var("PYTHON_SYS_EXECUTABLE ").unwrap_or_else(|_| "python.exe".to_owned());

    Command::new("powershell")
        .arg("-File")
        .arg("synclibs.ps1")
        .current_dir(&lib_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("synclibs failed");

    Command::new("powershell")
        .arg("-File")
        .arg("autogen.ps1")
        .current_dir(&lib_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("autogen failed");

    std::fs::remove_dir_all(&lib_path.join("vs2015")).unwrap();

    let lib_name = lib_path.file_name().unwrap().to_string_lossy().into_owned();

    Command::new(&python_exec)
        .arg("..\\..\\vstools\\scripts\\msvscpp-convert.py")
        .arg("--extend-with-x64")
        .arg("--output-format")
        .arg("2015")
        .arg(format!("msvscpp\\{}.sln", lib_name))
        .current_dir(&lib_path)
        .env("PYTHONPATH", "..\\..\\vstools")
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit())
        .status()
        .expect("Converting the solution failed. You might need to set PYTHON_SYS_EXECUTABLE to a working python interpreter");

    let target = env::var("TARGET").unwrap();

    let mut msbuild =
        cc::windows_registry::find(&target, "msbuild").expect("Needs msbuild installed");

    let msbuild_platform = if target.contains("x86_64") {
        "x64"
    } else {
        "Win32"
    };

    msbuild
        .arg(format!("vs2015\\{}.sln", lib_name))
        .arg("/property:PlatformToolset=v141")
        .arg(format!("/p:Platform={}", msbuild_platform))
        .current_dir(&lib_path)
        .stderr(Stdio::inherit())
        .stdout(Stdio::inherit());

    if !shared {
        msbuild.arg("/p:ConfigurationType=StaticLibrary");
    }

    msbuild.status().expect("Building the solution failed");

    let build_dir = lib_path
        .join("vs2015")
        .join("Release")
        .join(msbuild_platform);

    assert!(build_dir.exists(), "Expected {:?} to exist", build_dir);

    println!(
        "cargo:rustc-link-search=native={}",
        build_dir.to_string_lossy()
    );

    let include_folder_path = lib_path.join("include");

    // Override h files with bad encoding (anything created by autogen.ps1).
    let lib_h_file = include_folder_path.join(format!("{}.h", lib_name));
    utf16le_to_utf8(&lib_h_file).unwrap();

    let types_h_file = lib_path.join("common").join("types.h");
    utf16le_to_utf8(&types_h_file).unwrap();

    for file in ["definitions.h", "features.h", "types.h"].iter() {
        let file_path = include_folder_path.join(&lib_name).join(file);

        utf16le_to_utf8(&file_path).unwrap();
    }

    include_folder_path
}

#[cfg(target_os = "windows")]
fn utf16le_to_utf8(file_path: &PathBuf) -> Result<(), Error> {
    let h_file = File::open(&file_path)?;

    let mut transcoded = encoding_rs_io::DecodeReaderBytesBuilder::new()
        .encoding(Some(encoding_rs::UTF_16LE))
        .build(h_file);

    let mut content = String::new();

    transcoded.read_to_string(&mut content)?;

    drop(transcoded);

    let mut h_file = File::create(&file_path)?;
    h_file.write(content.as_bytes())?;

    Ok(())
}

fn build_and_link_static() -> PathBuf {
    let libbfio = if let Ok(local_install) = env::var("LIBBFIO_STATIC_LIBPATH") {
        PathBuf::from(local_install)
    } else {
        env::current_dir().unwrap().join("libbfio")
    };

    if cfg!(target_os = "windows") {
        println!("cargo:rustc-link-lib=static=libbfio");

        // Also static-link deps (otherwise we'll get missing symbols at link time).
        println!("cargo:rustc-link-lib=static=libcerror");
        println!("cargo:rustc-link-lib=static=libcdata");
        println!("cargo:rustc-link-lib=static=libcthreads");
    } else {
        println!("cargo:rustc-link-lib=static=bfio");
    }

    build_lib(libbfio, false)
}

fn build_and_link_dynamic() -> PathBuf {
    let libbfio = if let Ok(local_install) = env::var("LIBBFIO_DYNAMIC_LIBPATH") {
        PathBuf::from(local_install)
    } else {
        env::current_dir().unwrap().join("libbfio")
    };

    if cfg!(target_os = "windows") {
        println!("cargo:rustc-link-lib=dylib=libbfio");
    } else {
        println!("cargo:rustc-link-lib=dylib=bfio");
    }

    build_lib(libbfio, true)
}

fn main() {
    let include_folder_path = if cfg!(feature = "static_link") {
        println!("Building static bindings");
        build_and_link_static()
    } else {
        println!("Building dynamic bindings");
        build_and_link_dynamic()
    };

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // The input header we would like to generate
        // bindings for.
        .clang_args(&[format!("-I{}", include_folder_path.to_string_lossy())])
        .header("wrapper.h")
        // Finish the builder and generate the bindings.
        .generate()
        // Unwrap the Result and panic on failure.
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
