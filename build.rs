fn main() {
    println!("cargo:rerun-if-changed=assets/logitech-monitor.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/logitech-monitor.ico");
        resource.set("ProductName", "罗技电量管家");
        resource.set("FileDescription", "罗技设备电量监控托盘应用");
        resource.set("OriginalFilename", "logitech-monitor.exe");
        resource
            .compile()
            .expect("failed to compile Windows resources");
    }
}
