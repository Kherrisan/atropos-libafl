package instrumentor;

import api.HookType;
import org.objectweb.asm.AnnotationVisitor;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ClassVisitor;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;

import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.jar.JarEntry;
import java.util.jar.JarFile;

public final class HookScanner {
    private HookScanner() {}

    public static List<Hook> scan(String jarPath, String className) throws IOException {
        String entryName = className.replace('.', '/') + ".class";
        List<Hook> hooks = new ArrayList<>();
        try (JarFile jar = new JarFile(jarPath)) {
            JarEntry entry = jar.getJarEntry(entryName);
            if (entry == null) {
                return hooks;
            }
            try (InputStream input = jar.getInputStream(entry)) {
                ClassReader reader = new ClassReader(input);
                reader.accept(new Visitor(className, hooks), ClassReader.SKIP_CODE);
            }
        }
        return hooks;
    }

    private static boolean isHookAnnotation(String annotation) {
        return "Lapi/MethodHook;".equals(annotation)
                || "Lcom/code_intelligence/jazzer/api/MethodHook;".equals(annotation);
    }

    private static boolean isRepeatedHookAnnotation(String annotation) {
        return "Lapi/MethodHooks;".equals(annotation)
                || "Lcom/code_intelligence/jazzer/api/MethodHooks;".equals(annotation);
    }

    private static final class Visitor extends ClassVisitor {
        private final String className;
        private final List<Hook> hooks;

        private Visitor(String className, List<Hook> hooks) {
            super(Opcodes.ASM9);
            this.className = className;
            this.hooks = hooks;
        }

        @Override
        public MethodVisitor visitMethod(int access, String methodName, String methodDescriptor, String signature, String[] exceptions) {
            return new MethodVisitor(Opcodes.ASM9) {
                @Override
                public AnnotationVisitor visitAnnotation(String annotation, boolean visible) {
                    if (isHookAnnotation(annotation)) {
                        return new HookAnnotation(className, methodName, methodDescriptor, hooks);
                    }
                    if (isRepeatedHookAnnotation(annotation)) {
                        return new AnnotationVisitor(Opcodes.ASM9) {
                            @Override
                            public AnnotationVisitor visitArray(String arrayName) {
                                return new AnnotationVisitor(Opcodes.ASM9) {
                                    @Override
                                    public AnnotationVisitor visitAnnotation(String elementName, String elementDescriptor) {
                                        return new HookAnnotation(className, methodName, methodDescriptor, hooks);
                                    }
                                };
                            }
                        };
                    }
                    return null;
                }
            };
        }
    }

    private static final class HookAnnotation extends AnnotationVisitor {
        private final String className;
        private final String methodName;
        private final String methodDescriptor;
        private final List<Hook> hooks;
        private HookType type = HookType.BEFORE;
        private String targetClassName = "";
        private String targetMethod = "";
        private String targetMethodDescriptor = "";
        private final List<String> additional = new ArrayList<>();

        private HookAnnotation(String className, String methodName, String methodDescriptor, List<Hook> hooks) {
            super(Opcodes.ASM9);
            this.className = className;
            this.methodName = methodName;
            this.methodDescriptor = methodDescriptor;
            this.hooks = hooks;
        }

        @Override
        public void visit(String name, Object value) {
            if ("targetClassName".equals(name)) {
                targetClassName = (String) value;
            } else if ("targetMethod".equals(name)) {
                targetMethod = (String) value;
            } else if ("targetMethodDescriptor".equals(name)) {
                targetMethodDescriptor = (String) value;
            }
        }

        @Override
        public void visitEnum(String name, String descriptor, String value) {
            if ("type".equals(name)) {
                type = HookType.valueOf(value);
            }
        }

        @Override
        public AnnotationVisitor visitArray(String name) {
            if (!"additionalClassesToHook".equals(name)) {
                return null;
            }
            return new AnnotationVisitor(Opcodes.ASM9) {
                @Override
                public void visit(String ignored, Object value) {
                    additional.add((String) value);
                }
            };
        }

        @Override
        public void visitEnd() {
            hooks.add(Hook.Companion.createUnverified(
                    className,
                    methodName,
                    methodDescriptor,
                    type,
                    targetClassName,
                    targetMethod,
                    targetMethodDescriptor,
                    additional));
        }
    }
}
