fn main() {
    println!("cargo:rerun-if-changed=assets/battery-monitor.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/battery-monitor.ico");
        resource.set("ProductName", "电量管家");
        resource.set("FileDescription", "设备电量监控托盘应用");
        resource.set("OriginalFilename", "battery-monitor.exe");
        resource
            .compile()
            .expect("failed to compile Windows resources");
    }
}
