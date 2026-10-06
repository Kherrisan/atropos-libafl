package runtime;

final class NyxBitmap {
    static {
        System.loadLibrary("atropos_nyx_bitmap");
    }

    private NyxBitmap() {}

    static native void hit(int id);
}
