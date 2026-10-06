import fastcgi/client
import osproc
import std/[json, os, strformat, strutils]

{.compile: "nyx.c".}
{.compile: "nyx_dump_file.c".}

proc nyx_init(): void {.importc.}
proc nyx_create_snapshot(): void {.importc.}
proc nyx_exit(): void {.importc.}
proc nyx_get_payload(): cstring {.importc.}
proc nyx_get_payload_len(): uint32 {.importc.}
proc nyx_get_shm_id(): cint {.importc.}
proc nyx_get_bitmap_size(): uint32 {.importc.}
proc nyx_report_crash(message: cstring): void {.importc.}
proc nyx_hprintf(message: cstring): void {.importc.}
proc nyx_dump_file(name: cstring, data: pointer, len: uint32): void {.importc.}
proc nyx_coverage_dump(buffer: cstring, length: uint32, cpu: uint32, kind: uint8): void {.importc.}
proc nyx_capture_coverage_baseline(): cint {.importc.}
proc nyx_apply_coverage_baseline(): void {.importc.}

const
  PhpSocket = "/tmp/php.sock"
  PhpLog = "/tmp/php-cli.log"
  WebRoot = "/var/www/html"
  MaxRequestBytes = 1024 * 1024
  SecretReadTrigger = "secret4815162342"
  WarmupScripts = [
    "/var/www/html/index.php",
    "/var/www/html/wp-load.php",
    "/var/www/html/wp-blog-header.php",
    "/var/www/html/wp-login.php",
  ]

func createCrcTable(): array[0..255, uint32] =
  for i in 0'u32..255'u32:
    var remainder = i
    for _ in 0..7:
      if (remainder and 1) > 0'u32:
        remainder = (remainder shr 1) xor 0xedb88320'u32
      else:
        remainder = remainder shr 1
    result[i] = remainder

template updateCrc32(character: char; crc: var uint32) =
  crc = (crc shr 8) xor createCrcTable()[uint32(crc and 0xff) xor uint32(ord(character))]

func crc32(input: string): uint32 =
  var crc = 0xffffffff'u32
  for character in input:
    updateCrc32(character, crc)
  not crc

proc writeOracleCrc(): uint32 =
  const path = "/var/www/html/crash.php"
  if fileExists(path):
    let contents = readFile(path)
    return crc32(contents)
  0'u32

proc guestLog(message: string) =
  if message.len > 0:
    nyx_hprintf(message.cstring)

proc phpCliLogSize(): int =
  if not fileExists(PhpLog):
    return 0
  try:
    int(getFileSize(PhpLog))
  except CatchableError:
    0

proc publishPhpCliLog(startSize: int) =
  const hostName = "php-cli.log"
  const maxDump = 1024 * 1024
  if not fileExists(PhpLog):
    nyx_dump_file(hostName, nil, 0)
    return
  var contents = readFile(PhpLog)
  if startSize > 0:
    if startSize >= contents.len:
      contents.setLen(0)
    else:
      contents = contents.substr(startSize)
  if contents.len > maxDump:
    contents = contents.substr(contents.len - maxDump)
  if contents.len == 0:
    nyx_dump_file(hostName, nil, 0)
    return
  nyx_dump_file(hostName, unsafeAddr contents[0], uint32(contents.len))

proc startMariaDb(): bool =
  discard execCmd("chmod -R 777 /var/lib/php/sessions/")
  for attempt in 0 ..< 100:
    if execCmd("mariadb-admin --no-defaults --protocol=socket ping --silent >/dev/null 2>&1") == 0:
      return true
    sleep(100)
  false

proc startPhpCgi() =
  discard execCmd("pidof target_executable >/dev/null 2>&1 && kill $(pidof target_executable) || true; sleep 0.2 || true")
  discard execCmd("rm -f /tmp/php.sock /tmp/redqueen_mode_enabled /tmp/coverage_dump_enabled /tmp/execution_limit /tmp/bug_triggered /tmp/limit_reached /tmp/atropos-php-coverage-enabled")
  discard execCmd("rm -rf /tmp/atropos-php-coverage")
  let launch = fmt"IN_NYX=1 SHM_ID={nyx_get_shm_id()} BITMAP_SIZE={nyx_get_bitmap_size()} " &
    "SERVER_NAME=localhost REDIRECT_STATUS=1 PHP_FCGI_CHILDREN=0 PHP_FCGI_MAX_REQUESTS=0 " &
    "NYX_REPORT_LFI=1 NYX_INCLUDE_ERROR_IS_LFI=1 NYX_REPORT_EVAL=1 " &
    "NYX_REPORT_SQL_INJECTION=1 NYX_REPORT_UNSERIALIZE=1 " &
    "LD_LIBRARY_PATH=/tmp/ LD_BIND_NOW=1 " &
    fmt"/tmp/target_executable -b {PhpSocket} -c /tmp/php.ini >>{PhpLog} 2>&1 &"
  discard execCmd(launch)

proc phpSocketReady(): bool =
  # fileExists only accepts regular files, so a listening Unix socket looks missing.
  try:
    discard getFileInfo(PhpSocket)
    true
  except CatchableError:
    false

proc waitForPhpSocket(): bool =
  for attempt in 0 ..< 300:
    if phpSocketReady():
      return true
    sleep(100)
  false

proc scriptName(path: string): string =
  if path.startsWith(WebRoot):
    result = path.substr(WebRoot.len)
  else:
    result = path
  if result.len == 0 or result[0] != '/':
    result = "/" & result

proc responseBody(output: string): string =
  let parts = output.split("\r\n\r\n", 1)
  if parts.len > 1 and parts[1].len > 0:
    parts[1]
  else:
    output

proc sendFastCgi(request: JsonNode): string =
  let client = newFCGICLientUnix(PhpSocket)
  client.connect()
  var body = ""
  try:
    for key, value in request.pairs:
      if value.kind != JString:
        continue
      if key == "POST_DATA":
        body = value.getStr()
      elif not key.startsWith("_RAW"):
        client.setParam(key, value.getStr())
    result = client.sendRequest(body)
  finally:
    client.close()

proc warmupGet(script: string): bool =
  let name = scriptName(script)
  let request = %*{
    "REDIRECT_STATUS": "1",
    "SCRIPT_FILENAME": script,
    "SCRIPT_NAME": name,
    "REQUEST_METHOD": "GET",
    "QUERY_STRING": "",
    "CONTENT_LENGTH": "0",
    "CONTENT_TYPE": "application/x-www-form-urlencoded",
    "REQUEST_URI": name,
    "SERVER_PROTOCOL": "HTTP/1.1",
    "SERVER_NAME": "localhost",
    "SERVER_ADDR": "127.0.0.1",
    "SERVER_PORT": "8000",
    "HTTP_HOST": "localhost:8000",
    "HTTP_COOKIE": "",
  }
  try:
    discard sendFastCgi(request)
    true
  except CatchableError as error:
    guestLog(fmt"FastCGI warmup failed for {script}: {error.msg}\n")
    false

proc warmupOpcache(): bool =
  var warmedIndex = false
  for script in WarmupScripts:
    if not fileExists(script):
      continue
    if warmupGet(script):
      if script.endsWith("/index.php"):
        warmedIndex = true
      continue
    guestLog(fmt"Restarting php-cgi after warmup failure for {script}\n")
    startPhpCgi()
    if not waitForPhpSocket():
      return false
    if warmupGet(script) and script.endsWith("/index.php"):
      warmedIndex = true
  warmedIndex

proc flushPermalinks(): bool =
  # WordPress is the adapted app that rewrites permalinks before the snapshot.
  # Other PHP apps leave /tmp/atropos-app-id set to their name and skip this.
  if fileExists("/tmp/atropos-app-id") and readFile("/tmp/atropos-app-id").strip != "wordpress":
    return true
  let command = fmt"IN_NYX=1 SHM_ID={nyx_get_shm_id()} BITMAP_SIZE={nyx_get_bitmap_size()} " &
    "LD_LIBRARY_PATH=/tmp/ LD_BIND_NOW=1 " &
    "/tmp/php-cli -c /tmp/php.ini -d auto_prepend_file= -d auto_append_file= -d pcov.enabled=0 " &
    fmt"/tmp/atropos-flush-permalinks.php >>{PhpLog} 2>&1"
  execCmd(command) == 0

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
    # libafl_nyx starts with a non-input placeholder. Release it and acquire
    # the snapshot point again before accepting the first real testcase.
    nyx_exit()
    nyx_create_snapshot()

proc requestConfig(payload: JsonNode, key: string): string =
  if payload.hasKey("config") and payload["config"].kind == JObject and payload["config"].hasKey(key):
    let value = payload["config"][key]
    if value.kind == JString:
      return value.getStr()
    return $value
  ""

proc writeToggleFile(path, value: string) =
  if value.len == 0:
    discard tryRemoveFile(path)
  else:
    writeFile(path, value & "\0")

proc dumpCoverage(cpu: uint32) =
  const directory = "/tmp/atropos-php-coverage"
  let coberturaPath = directory & "/current.cobertura.xml"
  let serializedPath = directory & "/current.cov"
  let baselineCoberturaPath = directory & "/baseline.cobertura.xml"
  let baselineSerializedPath = directory & "/baseline.cov"
  if fileExists(coberturaPath):
    let cobertura = readFile(coberturaPath)
    nyx_coverage_dump(cobertura.cstring, uint32(cobertura.len), cpu, 0'u8)
  else:
    guestLog("PHP_CodeCoverage did not create current.cobertura.xml\n")
    let errorPath = directory & "/error.log"
    if fileExists(errorPath):
      guestLog(readFile(errorPath))
  if fileExists(serializedPath):
    let serialized = readFile(serializedPath)
    nyx_coverage_dump(serialized.cstring, uint32(serialized.len), cpu, 1'u8)
  else:
    guestLog("PHP_CodeCoverage did not create current.cov\n")
  if fileExists(baselineCoberturaPath):
    let baselineCobertura = readFile(baselineCoberturaPath)
    nyx_coverage_dump(baselineCobertura.cstring, uint32(baselineCobertura.len), cpu, 2'u8)
  else:
    guestLog("PHP_CodeCoverage did not create baseline.cobertura.xml\n")
  if fileExists(baselineSerializedPath):
    let baselineSerialized = readFile(baselineSerializedPath)
    nyx_coverage_dump(baselineSerialized.cstring, uint32(baselineSerialized.len), cpu, 3'u8)
  else:
    guestLog("PHP_CodeCoverage did not create baseline.cov\n")

proc reportCrashes(log: string) =
  var crashLog = log
  if fileExists("/tmp/bug_triggered"):
    crashLog.add(readFile("/tmp/bug_triggered"))
  if crashLog.len > 0:
    nyx_report_crash(crashLog.cstring)

proc main() =
  nyx_init()
  guestLog("Nyx initialization returned to Atropos agent\n")
  guestLog("Waiting for MariaDB before starting php-cgi\n")
  if not startMariaDb():
    guestLog("MariaDB did not become ready; aborting the guest agent\n")
    quit(1)

  discard execCmd("rm -f /tmp/php-cli.log")
  startPhpCgi()
  guestLog("Started php-cgi; waiting for /tmp/php.sock\n")
  if not waitForPhpSocket():
    guestLog("php-cgi did not listen on /tmp/php.sock\n")
    if fileExists(PhpLog):
      guestLog(readFile(PhpLog))
    quit(1)

  guestLog("Flushing permalinks before the Nyx snapshot\n")
  if not flushPermalinks():
    guestLog("Permalink flush failed\n")
    if fileExists(PhpLog):
      guestLog(readFile(PhpLog))
    quit(1)

  guestLog("Warming OPcache with FastCGI GET requests\n")
  if not warmupOpcache():
    guestLog("FastCGI warmup did not execute index.php\n")
    if fileExists(PhpLog):
      guestLog(readFile(PhpLog))
    quit(1)
  if not phpSocketReady():
    guestLog("php-cgi stopped listening after warmup\n")
    quit(1)

  guestLog("php-cgi is listening; acquiring Nyx snapshot\n")
  if nyx_capture_coverage_baseline() != 0:
    guestLog("Could not preserve the PCOV bootstrap coverage bitmap\n")
  let arbitraryWriteOracleCrcBefore = writeOracleCrc()
  let payload = parsePayloadWithSnapshotHandshake()
  if payload["requests"].len == 0:
    nyx_exit()
    quit(0)

  let coverageConfig = requestConfig(payload, "COVERAGE_DUMP")
  let detailedCoverage = coverageConfig.len > 0
  let coverageCpu = if detailedCoverage: uint32(parseInt(coverageConfig)) else: 0'u32
  nyx_apply_coverage_baseline()
  discard execCmd("rm -f /tmp/bug_triggered /tmp/limit_reached")
  writeToggleFile("/tmp/coverage_dump_enabled", coverageConfig)
  writeToggleFile("/tmp/execution_limit", requestConfig(payload, "EXEC_LIMIT"))
  if detailedCoverage:
    discard execCmd("mkdir -p /tmp/atropos-php-coverage; rm -f /tmp/atropos-php-coverage/current.cobertura.xml /tmp/atropos-php-coverage/current.cov /tmp/atropos-php-coverage/error.log")

  let phpLogStart = phpCliLogSize()
  var crashLog = ""
  for request in payload["requests"].items:
    if request.kind != JObject:
      continue
    var output = ""
    try:
      output = sendFastCgi(request)
    except CatchableError as error:
      guestLog(fmt"FastCGI request failed: {error.msg}\n")
      publishPhpCliLog(phpLogStart)
      nyx_exit()
      quit(0)

    let body = responseBody(output)
    let filename = if request.hasKey("SCRIPT_FILENAME") and request["SCRIPT_FILENAME"].kind == JString:
        request["SCRIPT_FILENAME"].getStr()
      else:
        "/var/www/html/index.php"
    let preview = if body.len > 180: body[0 .. 179] else: body
    guestLog(fmt"response_bytes={body.len} script={filename} body={preview}" & "\n")
    if SecretReadTrigger in body:
      crashLog.add(fmt"bug oracle triggered: validated arbitrary read in {filename}\n")
    let arbitraryWriteOracleCrcAfter = writeOracleCrc()
    if arbitraryWriteOracleCrcBefore != arbitraryWriteOracleCrcAfter:
      crashLog.add(fmt"bug oracle triggered: validated arbitrary file write/delete/rename in {filename}\n")
    if fileExists("/tmp/limit_reached"):
      break

  if detailedCoverage:
    dumpCoverage(coverageCpu)
  reportCrashes(crashLog)
  publishPhpCliLog(phpLogStart)
  nyx_exit()

when isMainModule:
  main()
