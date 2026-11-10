# R8 rules for AudioPush.
#
# The app uses no reflection, no serialization library and no dynamic class
# loading, so the defaults in proguard-android-optimize.txt cover almost
# everything. Only the entry points the platform instantiates by name need
# keeping, and the Android plugin already derives those from the manifest.

# Diagnostic logging is deliberately kept in release builds. "The meter moves
# but I hear nothing" has several causes that are indistinguishable from
# outside, and `adb logcat -s AudioPush` is how the README tells people to
# tell them apart. Stripping it would make the app harder to support, not
# smaller in any way that matters.
-keepclassmembers class com.kosherplay.audiopush.** {
    public *** TAG;
}

# Keep source file and line numbers in any stack trace that reaches us, and
# hide the original file name.
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
