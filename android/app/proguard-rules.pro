-keep class com.example.rustle.NativeWifiBridge { *; }
# Falls du weitere Klassen aus Rust per JNI aufrufst:
-keep class com.example.rustle.** { *; }