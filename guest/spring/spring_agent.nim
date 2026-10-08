import osproc
import std/[json, net, os, strformat, strutils]

{.compile: "nyx.c".}
{.compile: "nyx_dump_file.c".}

proc nyx_init(): void {.importc.}
proc nyx_create_snapshot(): void {.importc.}
proc nyx_exit(): void {.importc.}
proc nyx_get_payload(): cstring {.importc.}
proc nyx_get_payload_len(): uint32 {.importc.}
proc nyx_get_shm_id(): cint {.importc.}
proc nyx_get_bitmap_size(): uint32 {.importc.}
proc nyx_bitmap_set_count(): cint {.importc.}
proc nyx_report_crash(message: cstring): void {.importc.}
proc nyx_hprintf(message: cstring): void {.importc.}
proc nyx_dump_file(name: cstring, data: pointer, len: uint32): void {.importc.}

const
  SpringSocket = "/tmp/spring.sock"
  FuzzFlag = "/tmp/spring_fuzz_enabled"
  BugFile = "/tmp/bug_triggered"
  MaxRequestBytes = 1024 * 1024
  WebGoatUser = "atropos"
  WebGoatPassword = "atropos"

proc guestLog(message: string) =
  if message.len > 0:
    nyx_hprintf(message.cstring)

proc releaseToFuzzer(logCoverage: bool)

proc socketReady(path: string): bool =
  try:
    discard getFileInfo(path)
    true
  except CatchableError:
    false

proc waitForSocket(path: string): bool =
  for attempt in 0 ..< 6000:
    if socketReady(path):
      return true
    sleep(100)
  false

proc httpExchange(path, request: string): string =
  var socket = newSocket(AF_UNIX, SOCK_STREAM, IPPROTO_IP)
  socket.connectUnix(path)
  socket.send(request)
  var chunk = newString(8192)
  while true:
    let count = socket.recv(chunk, 8192)
    if count <= 0:
      break
    result.add(chunk[0 ..< count])
    if result.len > 8 * 1024 * 1024:
      break
  socket.close()

proc formRequest(verb, target, body, cookie: string): string =
  result = verb & " " & target & " HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n"
  if cookie.len > 0:
    result.add("Cookie: " & cookie & "\r\n")
  if body.len > 0:
    result.add("Content-Type: application/x-www-form-urlencoded\r\n")
    result.add("Content-Length: " & $body.len & "\r\n")
  result.add("\r\n")
  result.add(body)

proc responseHeader(response, name: string): string =
  let lowered = name.toLowerAscii()
  for line in response.split("\r\n"):
    let colon = line.find(':')
    if colon > 0 and line[0 ..< colon].toLowerAscii() == lowered:
      return line[colon + 1 .. ^1].strip()
  ""

proc sessionCookie(response, previous: string): string =
  result = previous
  for line in response.split("\r\n"):
    if line.toLowerAscii().startsWith("set-cookie:"):
      let value = line["set-cookie:".len .. ^1].strip()
      let pair = value.split(';', 1)[0].strip()
      if pair.toLowerAscii().startsWith("jsessionid="):
        result = pair

proc freshPayload(): string =
  let raw = nyx_get_payload()
  let payloadLength = int(nyx_get_payload_len())
  if raw == nil or payloadLength <= 0 or payloadLength > MaxRequestBytes:
    return ""
  result = newString(payloadLength)
  copyMem(addr result[0], raw, payloadLength)
  if result[^1] == '\0':
    result.setLen(result.len - 1)

proc parsePayloadWithSnapshotHandshake(): JsonNode =
  nyx_create_snapshot()
  while true:
    let payload = freshPayload()
    try:
      result = parseJson(payload)
      if result.kind == JObject and result.hasKey("requests") and result["requests"].kind == JArray:
        return
    except CatchableError:
      discard
    releaseToFuzzer(false)
    nyx_create_snapshot()

proc jsonString(node: JsonNode, key: string): string =
  if node.hasKey(key) and node[key].kind == JString:
    node[key].getStr()
  else:
    ""

proc headerName(key: string): string =
  if key.startsWith("HTTP_") and key != "HTTP_COOKIE":
    result = key[5 .. ^1].replace('_', '-')
  else:
    result = ""

proc buildRequest(request: JsonNode): string =
  let verb = jsonString(request, "REQUEST_METHOD")
  var uri = jsonString(request, "REQUEST_URI")
  if uri.len == 0:
    uri = "/"
  let body = jsonString(request, "POST_DATA")
  var contentType = jsonString(request, "CONTENT_TYPE")
  if contentType.len == 0:
    contentType = "application/json"
  # The fuzzer decides whether this input carries a cookie. HTTP_COOKIE is the
  # header it already built from the seed.
  let cookie = jsonString(request, "HTTP_COOKIE")
  result = verb & " " & uri & " HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n"
  if cookie.len > 0:
    result.add("Cookie: " & cookie & "\r\n")
  for key, value in request.pairs:
    if value.kind != JString:
      continue
    let name = headerName(key)
    if name.len > 0 and name.toLowerAscii() != "host":
      result.add(name & ": " & value.getStr() & "\r\n")
  if body.len > 0:
    result.add("Content-Type: " & contentType & "\r\n")
    result.add("Content-Length: " & $body.len & "\r\n")
  result.add("\r\n")
  result.add(body)

proc dumpText(name, text: string) =
  if text.len == 0:
    nyx_dump_file(name.cstring, nil, 0)
    return
  var copy = text
  nyx_dump_file(name.cstring, unsafeAddr copy[0], uint32(copy.len))

proc publishCoverage(logCoverage: bool) =
  if logCoverage:
    let covered = nyx_bitmap_set_count()
    let filter = if fileExists("/tmp/nyx-filter-ran"): "yes" else: "no"
    let other = if fileExists("/tmp/nyx-other-hit"): "yes" else: "no"
    var jniStatus = ""
    if fileExists("/tmp/nyx-bitmap-jni.txt"):
      jniStatus = readFile("/tmp/nyx-bitmap-jni.txt")
    dumpText("bitmap-count.txt", fmt"nyx={covered} filter={filter} other={other}")
    guestLog(fmt"bitmap set bytes nyx {covered} filter {filter} other {other} jni {jniStatus}")

proc releaseToFuzzer(logCoverage: bool) =
  publishCoverage(logCoverage)
  nyx_exit()

proc startWebGoat(shmId: cint, bitmapSize: uint32) =
  let home = "/var/lib/webgoat-home"
  discard execCmd("mkdir -p " & home & " /usr/local/lib/atropos-spring /tmp")
  discard tryRemoveFile(SpringSocket)
  discard tryRemoveFile(SpringSocket & ".webwolf")
  discard tryRemoveFile(FuzzFlag)
  discard tryRemoveFile(BugFile)
  let command = fmt"SHM_ID={shmId} BITMAP_SIZE={bitmapSize} ATROPOS_SPRING_SOCK={SpringSocket} " &
    "LD_LIBRARY_PATH=/usr/local/lib/atropos-spring/lib JAVA_HOME=/usr/local/lib/atropos-spring/jre " &
    "PATH=/usr/local/lib/atropos-spring/jre/bin:$PATH " &
    "/usr/local/lib/atropos-spring/jre/bin/java -Djava.library.path=/usr/local/lib/atropos-spring/lib " &
    "-Dloader.path=/usr/local/lib/atropos-spring/extra,/usr/local/lib/atropos-spring/springfuzz-hooks.jar " &
    "-Duser.home=" & home & " -Xmx2g " &
    "-javaagent:/usr/local/lib/atropos-spring/springfuzz-agent.jar=/usr/local/lib/atropos-spring/springfuzz-hooks.jar " &
    "-jar /usr/local/lib/atropos-spring/webgoat.jar >/tmp/webgoat.log 2>&1 &"
  discard execCmd(command)

proc loginWebGoat(): string =
  let body = "username=" & WebGoatUser & "&password=" & WebGoatPassword &
    "&matchingPassword=" & WebGoatPassword & "&agree=agree"
  var response = ""
  try:
    response = httpExchange(SpringSocket, formRequest("POST", "/WebGoat/register.mvc", body, ""))
  except CatchableError as error:
    guestLog(fmt"WebGoat registration failed: {error.msg}\n")
    return ""
  result = sessionCookie(response, "")
  if result.len == 0:
    try:
      response = httpExchange(SpringSocket, formRequest("POST", "/WebGoat/login",
        "username=" & WebGoatUser & "&password=" & WebGoatPassword, ""))
      result = sessionCookie(response, "")
    except CatchableError as error:
      guestLog(fmt"WebGoat login failed: {error.msg}\n")

proc warmup(session: string) =
  for target in [
      "/WebGoat/welcome.mvc",
      "/WebGoat/start.mvc",
      "/WebGoat/SqlInjection.lesson",
    ]:
    try:
      discard httpExchange(SpringSocket, formRequest("GET", target, "", session))
    except CatchableError as error:
      guestLog(fmt"Warmup {target} failed: {error.msg}\n")

proc exportOpenApi(session: string) =
  try:
    let response = httpExchange(SpringSocket, formRequest("GET", "/WebGoat/v3/api-docs", "", session))
    let parts = response.split("\r\n\r\n", 1)
    let body = if parts.len > 1: parts[1] else: response
    dumpText("openapi.json", body)
  except CatchableError as error:
    guestLog(fmt"OpenAPI export failed: {error.msg}\n")

proc publishSession(session: string) =
  var cookie = session
  if cookie.toLowerAscii().startsWith("jsessionid="):
    cookie = cookie["jsessionid=".len .. ^1]
  dumpText("session-cookie.txt", cookie)
  guestLog("ATROPOS_SPRING_SESSION JSESSIONID=" & cookie & "\n")

proc runAuthScript(): bool =
  # Project login runs once, after the JVM is listening. Warmup and OpenAPI
  # export do not read the cookie file.
  const path = "/usr/local/lib/atropos/auth.py"
  const logPath = "/tmp/atropos-auth.log"
  if not fileExists(path):
    return true
  guestLog("Running the project auth script after the JVM is listening\n")
  if execCmd("python3 " & path & " >" & logPath & " 2>&1") == 0:
    return true
  guestLog("Project auth script failed\n")
  if fileExists(logPath):
    guestLog(readFile(logPath))
  false

proc reportBugFile() =
  if not fileExists(BugFile):
    return
  let message = readFile(BugFile)
  if message.len > 0:
    nyx_report_crash(message.cstring)

proc main() =
  nyx_init()
  guestLog("Nyx initialization returned to the Spring agent\n")
  startWebGoat(nyx_get_shm_id(), nyx_get_bitmap_size())
  guestLog("Started WebGoat; waiting for /tmp/spring.sock\n")
  if not waitForSocket(SpringSocket):
    guestLog("WebGoat did not listen on /tmp/spring.sock\n")
    if fileExists("/tmp/webgoat.log"):
      guestLog(readFile("/tmp/webgoat.log"))
    quit(1)

  if fileExists("/usr/local/lib/atropos/auth.py"):
    if not runAuthScript():
      quit(1)
    warmup("")
    exportOpenApi("")
  else:
    let session = loginWebGoat()
    if session.len == 0:
      guestLog("WebGoat did not return a session cookie\n")
      if fileExists("/tmp/webgoat.log"):
        guestLog(readFile("/tmp/webgoat.log"))
      quit(1)
    warmup(session)
    exportOpenApi(session)
    publishSession(session)
  writeFile(FuzzFlag, "1")
  var jniStatus = "missing"
  if fileExists("/tmp/nyx-bitmap-jni.txt"):
    jniStatus = readFile("/tmp/nyx-bitmap-jni.txt")
  guestLog(fmt"bitmap before snapshot nyx {nyx_bitmap_set_count()} jni {jniStatus}")
  guestLog("WebGoat is listening; acquiring Nyx snapshot\n")
  let payload = parsePayloadWithSnapshotHandshake()
  if payload["requests"].len == 0:
    releaseToFuzzer(true)
    quit(0)

  discard tryRemoveFile(BugFile)
  # The snapshot restores the warmup marker. Remove it so only this input counts.
  discard tryRemoveFile("/tmp/nyx-filter-ran")
  discard tryRemoveFile("/tmp/nyx-other-hit")
  for request in payload["requests"].items:
    if request.kind != JObject:
      continue
    try:
      discard httpExchange(SpringSocket, buildRequest(request))
    except CatchableError as error:
      guestLog(fmt"HTTP request failed: {error.msg}\n")
      releaseToFuzzer(true)
      quit(0)
  reportBugFile()
  nyx_exit()

when isMainModule:
  main()
