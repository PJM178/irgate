//! Diagnostic: show every V4L2 node, whether it looks like and opens as an IR
//! camera, and which one authentication would actually use.
fn main() {
    println!("Scanning /sys/class/video4linux ...\n");
    for node in irgate_core::capture::list_video_nodes() {
        println!("{:<14} name={:?}", node.path, node.name);
        println!("  {:<24} {}", "name looks like IR:", node.ir_name);
        match &node.grey_capture {
            Ok(()) => println!("  {:<24} yes (GREY capture)", "opens as IR device:"),
            Err(e) => println!("  {:<24} no — {e}", "opens as IR device:"),
        }
        if node.name.len() >= 31 && !node.ir_name {
            println!("  note: the kernel truncates card names to 31 characters; an \"IR\" suffix may be cut off");
        }
        println!();
    }
    println!("enumerate_ir_cameras() -> {:?}", irgate_core::capture::enumerate_ir_cameras());
    println!("detect_ir_camera()     -> {:?}", irgate_core::capture::detect_ir_camera());
}
