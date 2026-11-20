# CoreBridge/CorePaymentParser reach the UniFFI bindings by reflection.
-keep class uniffi.girocrab_core.** { *; }
-keep class com.sun.jna.** { *; }

# JNA references desktop AWT/Swing for window handles it never needs on Android.
# Without this rule R8 aborts assembleRelease with "Missing class java.awt.Component".
-dontwarn java.awt.**
-dontwarn javax.swing.**
