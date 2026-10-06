#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
FUZZER_OUTPUT=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--fuzzer-output)
		FUZZER_OUTPUT="${2:?--fuzzer-output needs a directory}"
		shift 2
		;;
	*)
		printf 'unknown argument: %s\n' "$1" >&2
		exit 1
		;;
	esac
done
if [[ -z "$FUZZER_OUTPUT" ]]; then
	printf 'usage: build-nyx-spring.sh --fuzzer-output DIR\n' >&2
	exit 1
fi
SPRINGFUZZ_ROOT="$REPO_ROOT/third-party/SpringFuzz"
DATA_DIR="$FUZZER_OUTPUT/spring"
GUEST_DIR="$DATA_DIR/guest"
BUILD_ROOT="${ATROPOS_SPRING_BUILD_ROOT:-$(mktemp -d "${TMPDIR:-/tmp}/atropos-spring.XXXXXX")}"
MAVEN_VERSION="${ATROPOS_MAVEN_VERSION:-3.9.9}"
MAVEN_HOME="${ATROPOS_MAVEN_HOME:-$HOME/.local/opt/apache-maven-$MAVEN_VERSION}"
WEBGOAT_URL="${ATROPOS_WEBGOAT_URL:-https://github.com/WebGoat/WebGoat/releases/download/v2023.8/webgoat-2023.8.jar}"
JAVA_BIN="$(readlink -f "$(command -v java)")"
JAVA_HOME="$(cd -- "$(dirname -- "$JAVA_BIN")/.." && pwd)"

if [[ -z "${https_proxy:-}${HTTPS_PROXY:-}" ]]; then
	for candidate in http://127.0.0.1:9870 http://172.17.0.1:9870; do
		if curl --fail --silent --output /dev/null --connect-timeout 3 --proxy "$candidate" \
			https://repo.maven.apache.org/maven2/; then
			export http_proxy="$candidate" https_proxy="$candidate"
			export HTTP_PROXY="$candidate" HTTPS_PROXY="$candidate"
			break
		fi
	done
fi
MAVEN_SETTINGS=""
if [[ -n "${https_proxy:-}" ]]; then
	proxy_host="${https_proxy#*://}"
	proxy_host="${proxy_host%%/*}"
	proxy_port="${proxy_host##*:}"
	proxy_host="${proxy_host%%:*}"
	export MAVEN_OPTS="${MAVEN_OPTS:-} -Dhttp.proxyHost=$proxy_host -Dhttp.proxyPort=$proxy_port -Dhttps.proxyHost=$proxy_host -Dhttps.proxyPort=$proxy_port"
	MAVEN_SETTINGS="$BUILD_ROOT/maven-settings.xml"
	mkdir -p "$BUILD_ROOT"
	cat >"$MAVEN_SETTINGS" <<EOF
<settings>
  <proxies>
    <proxy>
      <id>atropos</id>
      <active>true</active>
      <protocol>http</protocol>
      <host>${proxy_host}</host>
      <port>${proxy_port}</port>
    </proxy>
  </proxies>
  <mirrors>
    <mirror>
      <id>central</id>
      <url>https://repo.maven.apache.org/maven2</url>
      <mirrorOf>*</mirrorOf>
    </mirror>
  </mirrors>
</settings>
EOF
	printf 'Using download proxy %s\n' "$https_proxy"
fi
MVN_SETTINGS_ARGS=()
if [[ -n "$MAVEN_SETTINGS" ]]; then
	MVN_SETTINGS_ARGS=(-s "$MAVEN_SETTINGS")
fi

if [[ ! -f "$SPRINGFUZZ_ROOT/pom.xml" ]]; then
	printf 'SpringFuzz sources are missing at %s\n' "$SPRINGFUZZ_ROOT" >&2
	exit 1
fi
if [[ ! -f "$REPO_ROOT/guest/common/nyx.h" ]]; then
	printf 'Nyx headers are missing at %s\n' "$REPO_ROOT/guest/common/nyx.h" >&2
	exit 1
fi
for dependency in curl python3 gcc unzip; do
	if ! command -v "$dependency" >/dev/null 2>&1; then
		printf 'Missing build command: %s\n' "$dependency" >&2
		exit 1
	fi
done

mkdir -p "$GUEST_DIR/extra" "$GUEST_DIR/lib" "$BUILD_ROOT"
chmod 700 "$DATA_DIR" "$GUEST_DIR"

if [[ ! -x "$MAVEN_HOME/bin/mvn" ]]; then
	mkdir -p "$(dirname -- "$MAVEN_HOME")"
	curl --fail --location --retry 3 \
		"https://repo.maven.apache.org/maven2/org/apache/maven/apache-maven/$MAVEN_VERSION/apache-maven-$MAVEN_VERSION-bin.tar.gz" \
		| tar -xz -C "$(dirname -- "$MAVEN_HOME")"
fi
MVN="$MAVEN_HOME/bin/mvn"

printf 'Building the SpringFuzz agent\n'
rm -rf "$BUILD_ROOT/springfuzz"
mkdir -p "$BUILD_ROOT/springfuzz"
cp -a "$SPRINGFUZZ_ROOT/." "$BUILD_ROOT/springfuzz/"
rm -rf "$BUILD_ROOT/springfuzz/target" "$BUILD_ROOT/springfuzz/.git"
for patch in "$REPO_ROOT"/guest/spring/patches/*.patch; do
	patch -p1 -d "$BUILD_ROOT/springfuzz" <"$patch"
done
mkdir -p "$BUILD_ROOT/springfuzz/src/main/java/runtime" \
	"$BUILD_ROOT/springfuzz/src/main/java/instrumentor"
cp -- "$REPO_ROOT/guest/spring/src/runtime/NyxBitmap.java" \
	"$BUILD_ROOT/springfuzz/src/main/java/runtime/NyxBitmap.java"
cp -- "$REPO_ROOT/guest/spring/src/instrumentor/HookScanner.java" \
	"$BUILD_ROOT/springfuzz/src/main/java/instrumentor/HookScanner.java"
if [[ -f "$BUILD_ROOT/springfuzz/src/main/java/init/returnFilter.java" ]]; then
	mv -- "$BUILD_ROOT/springfuzz/src/main/java/init/returnFilter.java" \
		"$BUILD_ROOT/springfuzz/src/main/java/init/ReturnFilter.java"
fi
(cd "$BUILD_ROOT/springfuzz" && "$MVN" -B "${MVN_SETTINGS_ARGS[@]}" kotlin:compile package -DskipTests)
python3 - "$BUILD_ROOT/springfuzz/target/SpringFuzz-1.0-SNAPSHOT.jar" \
	"$GUEST_DIR/springfuzz-agent.jar" "$GUEST_DIR/springfuzz-hooks.jar" <<'PY'
import zipfile
from pathlib import Path
source, agent_path, hooks_path = map(Path, __import__("sys").argv[1:])
hook_prefixes = ("init/", "sanitizers/")
keep_in_agent = (
    "init/Prepare.class",
    "agent/",
    "api/",
    "instrumentor/",
    "runtime/",
    "utils/",
    "org/objectweb/asm/",
    "org/jacoco/",
    "io/github/classgraph/",
    "nonapi/",
    "kotlin/",
    "kotlinx/",
    "javassist/",
    "META-INF/MANIFEST.MF",
    "META-INF/services/java.lang.instrument",
)
with zipfile.ZipFile(source) as inbound, \
        zipfile.ZipFile(agent_path, "w") as agent, \
        zipfile.ZipFile(hooks_path, "w") as hooks:
    for info in inbound.infolist():
        data = inbound.read(info.filename)
        copied = zipfile.ZipInfo(filename=info.filename, date_time=info.date_time)
        copied.compress_type = info.compress_type
        copied.external_attr = info.external_attr
        if info.filename.startswith(hook_prefixes + ("net/sf/jsqlparser/", "org/owasp/html/")):
            hooks.writestr(copied, data)
        if info.filename.startswith(keep_in_agent):
            agent.writestr(copied, data)
if b"Premain-Class" not in zipfile.ZipFile(agent_path).read("META-INF/MANIFEST.MF"):
    raise SystemExit("agent jar lost Premain-Class")
PY

printf 'Compiling Jazzer vulnerability oracles\n'
JAZZER_ROOT="$REPO_ROOT/third-party/jazzer"
JAZZER_CLASSES="$BUILD_ROOT/jazzer-classes"
JSQLPARSER_JAR="$(find "$HOME/.m2/repository/com/github/jsqlparser/jsqlparser" -name 'jsqlparser-*.jar' | sort | tail -1)"
KOTLINC_JAR="${ATROPOS_KOTLINC_JAR:-$HOME/.m2/repository/org/jetbrains/kotlin/kotlin-compiler-embeddable/1.9.24/kotlin-compiler-embeddable-1.9.24.jar}"
KOTLIN_STDLIB="${ATROPOS_KOTLIN_STDLIB:-$HOME/.m2/repository/org/jetbrains/kotlin/kotlin-stdlib/1.9.24/kotlin-stdlib-1.9.24.jar}"
KOTLIN_SCRIPT_RUNTIME="${ATROPOS_KOTLIN_SCRIPT_RUNTIME:-$HOME/.m2/repository/org/jetbrains/kotlin/kotlin-script-runtime/1.9.24/kotlin-script-runtime-1.9.24.jar}"
KOTLIN_REFLECT="${ATROPOS_KOTLIN_REFLECT:-$HOME/.m2/repository/org/jetbrains/kotlin/kotlin-reflect/1.6.10/kotlin-reflect-1.6.10.jar}"
TROVE_JAR="${ATROPOS_TROVE_JAR:-$HOME/.m2/repository/org/jetbrains/intellij/deps/trove4j/1.0.20200330/trove4j-1.0.20200330.jar}"
ANNOTATIONS_JAR="${ATROPOS_ANNOTATIONS_JAR:-$HOME/.m2/repository/org/jetbrains/annotations/24.1.0/annotations-24.1.0.jar}"
if [[ ! -f "$KOTLINC_JAR" || ! -f "$KOTLIN_STDLIB" || ! -f "$TROVE_JAR" || ! -f "$ANNOTATIONS_JAR" ]]; then
	"$MVN" -B "${MVN_SETTINGS_ARGS[@]}" -q dependency:get -Dartifact=org.jetbrains.kotlin:kotlin-compiler-embeddable:1.9.24
	"$MVN" -B "${MVN_SETTINGS_ARGS[@]}" -q dependency:get -Dartifact=org.jetbrains.kotlin:kotlin-stdlib:1.9.24
	"$MVN" -B "${MVN_SETTINGS_ARGS[@]}" -q dependency:get -Dartifact=org.jetbrains.kotlin:kotlin-script-runtime:1.9.24
	"$MVN" -B "${MVN_SETTINGS_ARGS[@]}" -q dependency:get -Dartifact=org.jetbrains.kotlin:kotlin-reflect:1.6.10
	"$MVN" -B "${MVN_SETTINGS_ARGS[@]}" -q dependency:get -Dartifact=org.jetbrains.intellij.deps:trove4j:1.0.20200330
	"$MVN" -B "${MVN_SETTINGS_ARGS[@]}" -q dependency:get -Dartifact=org.jetbrains:annotations:24.1.0
fi
if [[ ! -d "$JAZZER_ROOT/sanitizers" ]]; then
	printf 'Jazzer checkout is missing at %s\n' "$JAZZER_ROOT" >&2
	exit 1
fi
if [[ ! -f "$JSQLPARSER_JAR" || ! -f "$KOTLINC_JAR" ]]; then
	printf 'jsqlparser or kotlin-compiler jar is missing\n' >&2
	exit 1
fi
rm -rf "$JAZZER_CLASSES"
mkdir -p "$JAZZER_CLASSES"
javac --release 11 -d "$JAZZER_CLASSES" \
	"$JAZZER_ROOT"/src/main/java/com/code_intelligence/jazzer/api/HookType.java \
	"$JAZZER_ROOT"/src/main/java/com/code_intelligence/jazzer/api/MethodHook.java \
	"$JAZZER_ROOT"/src/main/java/com/code_intelligence/jazzer/api/MethodHooks.java \
	"$JAZZER_ROOT"/src/main/java/com/code_intelligence/jazzer/api/FuzzerSecurityIssueCritical.java \
	"$JAZZER_ROOT"/src/main/java/com/code_intelligence/jazzer/api/FuzzerSecurityIssueHigh.java \
	"$JAZZER_ROOT"/src/main/java/com/code_intelligence/jazzer/api/FuzzerSecurityIssueMedium.java \
	"$JAZZER_ROOT"/src/main/java/com/code_intelligence/jazzer/api/FuzzerSecurityIssueLow.java \
	"$REPO_ROOT"/guest/spring/jazzer-bridge/com/code_intelligence/jazzer/api/Jazzer.java
javac --release 11 -cp "$JAZZER_CLASSES:$JSQLPARSER_JAR" -d "$JAZZER_CLASSES" \
	"$JAZZER_ROOT"/src/main/java/jaz/Ter.java \
	"$JAZZER_ROOT"/src/main/java/jaz/Zer.java \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/SqlInjection.java \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/FilePathTraversal.java \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/ServerSideRequestForgery.java \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/ScriptEngineInjection.java
java -cp "$KOTLINC_JAR:$KOTLIN_STDLIB:$KOTLIN_SCRIPT_RUNTIME:$KOTLIN_REFLECT:$TROVE_JAR:$ANNOTATIONS_JAR" \
	org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -no-stdlib -no-reflect -jvm-target 11 \
	-cp "$JAZZER_CLASSES:$JSQLPARSER_JAR:$KOTLIN_STDLIB" -d "$JAZZER_CLASSES" \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/Utils.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/OsCommandInjection.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/LdapInjection.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/XPathInjection.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/ExpressionLanguageInjection.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/TemplateInjection.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/Deserialization.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/ReflectiveCall.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/NamingContextLookup.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/RegexInjection.kt \
	"$JAZZER_ROOT"/sanitizers/src/main/java/com/code_intelligence/jazzer/sanitizers/XmlParserSsrfGuidance.kt
(cd "$JAZZER_CLASSES" && jar uf "$GUEST_DIR/springfuzz-hooks.jar" com/code_intelligence jaz)
KOTLIN_STDLIB_DIR="$BUILD_ROOT/kotlin-stdlib-classes"
rm -rf "$KOTLIN_STDLIB_DIR"
mkdir -p "$KOTLIN_STDLIB_DIR"
(cd "$KOTLIN_STDLIB_DIR" && jar xf "$KOTLIN_STDLIB" kotlin)
(cd "$KOTLIN_STDLIB_DIR" && jar uf "$GUEST_DIR/springfuzz-hooks.jar" kotlin)

printf 'Building the Nyx bitmap library\n'
gcc -shared -fPIC -O2 \
	-I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" \
	-o "$GUEST_DIR/lib/libatropos_nyx_bitmap.so" \
	"$REPO_ROOT/guest/spring/nyx_bitmap.c"
gcc -O2 -o "$GUEST_DIR/shm-tool" "$REPO_ROOT/guest/spring/shm_tool.c"

printf 'Building the springdoc and OpenAPI security helper\n'
(cd "$REPO_ROOT/guest/spring/support" && "$MVN" -B "${MVN_SETTINGS_ARGS[@]}" -DskipTests package \
	dependency:copy-dependencies -DincludeScope=runtime \
	"-DoutputDirectory=$BUILD_ROOT/spring-extra")
cp -- "$REPO_ROOT/guest/spring/support/target/atropos-spring-support-1.0.0.jar" \
	"$BUILD_ROOT/spring-extra/"

if [[ ! -f "$GUEST_DIR/webgoat-2023.8.jar" ]]; then
	curl --fail --location --retry 3 --output "$GUEST_DIR/webgoat-2023.8.jar.part" "$WEBGOAT_URL"
	mv -- "$GUEST_DIR/webgoat-2023.8.jar.part" "$GUEST_DIR/webgoat-2023.8.jar"
fi
rm -rf "$GUEST_DIR/extra"
mkdir -p "$GUEST_DIR/extra"
for jar in "$BUILD_ROOT"/spring-extra/*.jar; do
	base="$(basename -- "$jar")"
	case "$base" in
		springdoc-*|swagger-*|atropos-spring-support-*|jackson-*|jakarta.validation-api-*|tomcat-embed-core-*|tomcat-embed-websocket-*)
			cp -- "$jar" "$GUEST_DIR/extra/"
			;;
	esac
done
cp -- "$GUEST_DIR/springfuzz-hooks.jar" "$GUEST_DIR/extra/springfuzz-hooks.jar"
python3 - "$GUEST_DIR/springfuzz-agent.jar" "$BUILD_ROOT/spring-extra" <<'PY'
import sys
import zipfile
from pathlib import Path

agent_path = Path(sys.argv[1])
extra = Path(sys.argv[2])
jackson = [
    extra / "jackson-annotations-2.15.2.jar",
    extra / "jackson-core-2.15.2.jar",
    extra / "jackson-databind-2.15.2.jar",
]
missing = [path for path in jackson if not path.is_file()]
if missing:
    raise SystemExit("missing Jackson jars: " + ", ".join(str(path) for path in missing))
original = agent_path.read_bytes()
temporary = agent_path.with_suffix(".jar.tmp")
with zipfile.ZipFile(agent_path) as agent:
    present = set(agent.namelist())
with zipfile.ZipFile(temporary, "w") as outbound, zipfile.ZipFile(agent_path) as agent:
    for info in agent.infolist():
        outbound.writestr(info, agent.read(info.filename))
    for path in jackson:
        with zipfile.ZipFile(path) as library:
            for info in library.infolist():
                if info.is_dir() or info.filename in present or info.filename.startswith("META-INF/"):
                    continue
                present.add(info.filename)
                outbound.writestr(info.filename, library.read(info.filename))
temporary.replace(agent_path)
print(f"added Jackson to {agent_path}")
PY
if ! command -v zip >/dev/null 2>&1; then
	printf 'Missing build command: zip\n' >&2
	exit 1
fi
# Rewrite the fat jar from scratch and JRuby can no longer see the Asciidoctor gem.
# Append stored entries onto the original Spring Boot archive instead.
python3 - "$GUEST_DIR/webgoat-2023.8.jar" "$GUEST_DIR/webgoat.jar" "$GUEST_DIR/extra" <<'PY'
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
extra_dir = Path(sys.argv[3])
with zipfile.ZipFile(source) as archive:
    manifest = archive.read("META-INF/MANIFEST.MF").decode()
    if "Main-Class: org.springframework.boot.loader.JarLauncher" not in manifest:
        raise SystemExit("WebGoat manifest does not use JarLauncher")
    existing = {
        Path(name).name
        for name in archive.namelist()
        if name.startswith("BOOT-INF/lib/") and name.endswith(".jar")
    }
    index = archive.read("BOOT-INF/classpath.idx").decode()
additions = sorted(
    path for path in extra_dir.glob("*.jar") if path.stat().st_size and path.name not in existing
)
if not index.endswith("\n"):
    index += "\n"
for path in additions:
    index += f'- "BOOT-INF/lib/{path.name}"\n'
destination.write_bytes(source.read_bytes())
with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    (root / "BOOT-INF").mkdir()
    (root / "BOOT-INF/classpath.idx").write_text(index)
    library = root / "BOOT-INF/lib"
    library.mkdir()
    for path in additions:
        (library / path.name).write_bytes(path.read_bytes())
    subprocess.run(
        ["zip", "-0", "-X", str(destination), "BOOT-INF/classpath.idx", *[
            f"BOOT-INF/lib/{path.name}" for path in additions
        ]],
        cwd=root,
        check=True,
    )
print(f"merged {len(additions)} jar(s) into {destination}")
PY

if [[ ! -x "$GUEST_DIR/jre/bin/java" ]]; then
	rm -rf "$GUEST_DIR/jre"
	mkdir -p "$GUEST_DIR/jre"
	cp -a "$JAVA_HOME/." "$GUEST_DIR/jre/"
fi

printf 'Building the Spring Nyx agent\n'
"$SCRIPT_DIR/with-nyx-build-deps.sh" bash -s -- "$BUILD_ROOT" "$GUEST_DIR" "$REPO_ROOT" <<'BASH'
set -euo pipefail
build_root="$1"
guest_dir="$2"
repo_root="$3"
agent_source="$build_root/agent-src"
mkdir -p "$agent_source"
cp -- "$repo_root/guest/spring/spring_agent.nim" "$repo_root/guest/common/nyx_dump_file.c" \
	"$repo_root/guest/common/nyx.c" "$repo_root/guest/common/nyx.h" "$agent_source/"
python3 - "$agent_source/nyx.c" <<'PYTHON'
from pathlib import Path
import sys
path = Path(sys.argv[1])
source = path.read_text()
anchor = "        kAFL_hypercall(HYPERCALL_KAFL_GET_PAYLOAD, (uintptr_t)payload_buffer);\n"
if source.count(anchor) != 1:
    raise SystemExit("could not locate the Nyx payload initialization anchor")
length_function = """uint32_t nyx_get_payload_len() {
    return payload_buffer->size - sizeof(payload_buffer->size);
}"""
if source.count(length_function) != 1:
    raise SystemExit("could not locate the legacy Nyx payload-length helper")
source = source.replace(length_function, """uint32_t nyx_get_payload_len() {
    /* libnyx stores the input byte count here; it is not the struct size. */
    return payload_buffer->size;
}""")
source = source.replace(anchor, anchor + "        done = true;\n")
if "#include <fcntl.h>" not in source:
    source = source.replace(
        "#include <sys/shm.h>\n",
        "#include <sys/shm.h>\n#include <fcntl.h>\n#include <unistd.h>\n",
        1,
    )
snapshot = """void nyx_create_snapshot() {
    kAFL_hypercall(HYPERCALL_KAFL_USER_FAST_ACQUIRE, 0); // root snapshot <--
    ((uint8_t*)trace_buffer)[0] = 0x1;
}"""
if source.count(snapshot) != 1:
    raise SystemExit("could not locate nyx_create_snapshot")
source = source.replace(snapshot, """static uint32_t edge_epoch_value;

static void nyx_touch_edge_epoch(void) {
    char buf[16];
    int fd;
    int len;
    /* This runs after the snapshot is restored, so the counter value stored
       in the snapshot is incremented again on every execution. */
    edge_epoch_value++;
    len = snprintf(buf, sizeof(buf), "%u", edge_epoch_value);
    fd = open("/tmp/nyx-edge-epoch", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd >= 0) {
        if (len < 1 || write(fd, buf, (size_t)len) != len) {
            hprintf("edge epoch write failed\\n");
        }
        close(fd);
    }
}

void nyx_create_snapshot() {
    kAFL_hypercall(HYPERCALL_KAFL_USER_FAST_ACQUIRE, 0); // root snapshot <--
    /* Runs again on every restore. Drop the previous execution before Java
       handles this input. Hits write straight into trace_buffer. */
    if (host_config != NULL && trace_buffer != NULL && trace_buffer != (void *)-1) {
        memset(trace_buffer, 0, host_config->bitmap_size);
    }
    nyx_touch_edge_epoch();
}
""")
path.write_text(source)
PYTHON
(
	cd "$agent_source" && env -u LD_LIBRARY_PATH nim c \
		--passC:-B/usr/bin/ --passL:-B/usr/bin/ \
		--nimcache:"$build_root/nimcache" \
		--d:release --opt:speed \
		--out:"$guest_dir/atropos_spring_agent" \
		spring_agent.nim
)
BASH
patchelf_bin="$(command -v patchelf || true)"
if [[ -z "$patchelf_bin" ]]; then
	printf 'patchelf is required so the Spring agent uses /lib64/ld-linux-x86-64.so.2\n' >&2
	exit 1
fi
"$patchelf_bin" --set-interpreter /lib64/ld-linux-x86-64.so.2 "$GUEST_DIR/atropos_spring_agent"
chmod 0755 "$GUEST_DIR/atropos_spring_agent" "$GUEST_DIR/shm-tool"
printf 'spring-nyx-webgoat-2023.8\n' >"$GUEST_DIR/spring-runtime"
printf 'Spring Nyx guest artifacts: %s\n' "$GUEST_DIR"
