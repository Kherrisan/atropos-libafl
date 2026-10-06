package com.code_intelligence.jazzer.api;

import java.io.FileWriter;
import java.io.IOException;

/**
 * Stand-in for Jazzer's runtime. Sanitizers call this class; findings are appended to the
 * file the Nyx agent already treats as a crash.
 */
public final class Jazzer {
    private Jazzer() {}

    public static void reportFindingFromHook(Throwable finding) {
        if (finding == null) {
            return;
        }
        String type = finding.getClass().getSimpleName();
        String message = finding.getMessage() == null ? "" : finding.getMessage();
        try (FileWriter writer = new FileWriter("/tmp/bug_triggered", true)) {
            writer.write(type);
            writer.write('\n');
            writer.write(message);
            if (!message.endsWith("\n")) {
                writer.write('\n');
            }
        } catch (IOException ignored) {
            // The request still returns; the agent only sees findings that were written.
        }
    }

    public static void guideTowardsEquality(String current, String target, int id) {}

    public static void guideTowardsEquality(byte[] current, byte[] target, int id) {}

    public static void guideTowardsContainment(String haystack, String needle, int id) {}

    public static void onFuzzTargetReady(Runnable callback) {
        if (callback != null) {
            callback.run();
        }
    }
}
