use std::env;
use std::path::PathBuf;

fn main() {
    // let header_dir = PathBuf::from("/Applications/MCUXpressoIDE_11.7.1_9221//ide/plugins/com.nxp.mcuxpresso.tools.macosx_11.7.1.202301190959/tools/arm-none-eabi/include");
    // Add the header directory to the include path
    // println!("cargo:include={}", header_dir.display());

    // Set the path to the directory containing the C library
    let current_dir = env::current_dir().unwrap();
    // let pqc_path = current_dir.join("../../pqm4/crypto_sign/dilithium3/m4f");

    // Tell the build script to link against the C library
    println!("cargo:rustc-link-search=native={}", current_dir.display());
    // println!("cargo:rustc-link-lib=static=dilithium3_clean");

    // println!("cargo:rustc-link-search=native=/Applications/MCUXpressoIDE_11.7.1_9221/ide/plugins/com.nxp.mcuxpresso.tools.macosx_11.7.1.202301190959/tools/arm-none-eabi/lib/thumb/v8-m.main+fp/hard/");
    // println!("cargo:rustc-link-lib=static=c");
    // /Applications/MCUXpressoIDE_11.7.1_9221/ide/plugins/com.nxp.mcuxpresso.tools.macosx_11.7.1.202301190959/tools/arm-none-eabi/lib/thumb/v8-m.main+fp/hard/libc.a
    // println!("cargo:rustc-link-lib=static=pqm4_duc_dilithium3");
    println!("cargo:rustc-link-lib=static=pqc_lib");
}

// fn main() {
//     // Tell cargo to look for shared libraries in the specified directory
//     let current_dir = env::current_dir().unwrap();
//     println!("cargo:rustc-link-search=native={}", current_dir.display());

//     // Tell cargo to tell rustc to link the system bzip2
//     // shared library.
//     println!("cargo:rustc-link-lib=pqm4_duc_dilithium3");

//     // Tell cargo to invalidate the built crate whenever the wrapper changes
//     println!("cargo:rerun-if-changed=wrapper.h");

//     // The bindgen::Builder is the main entry point
//     // to bindgen, and lets you build up options for
//     // the resulting bindings.
//     let bindings = bindgen::Builder::default()
//         // The input header we would like to generate
//         // bindings for.
//         .header("wrapper.h")
//         // Tell cargo to invalidate the built crate whenever any of the
//         // included header files changed.
//         .parse_callbacks(Box::new(bindgen::CargoCallbacks))
//         // Finish the builder and generate the bindings.
//         .generate()
//         // Unwrap the Result and panic on failure.
//         .expect("Unable to generate bindings");

//     // Write the bindings to the $OUT_DIR/bindings.rs file.
//     let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
//     bindings
//         .write_to_file(out_path.join("bindings.rs"))
//         .expect("Couldn't write bindings!");
// }
