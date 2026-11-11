fn main() {
    // No std-widgets are used: every widget is themed by hand. The software renderer is chosen
    // at runtime (see README for why).
    slint_build::compile("ui/main.slint").expect("slint compile");
}
