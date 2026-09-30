import osproc
import std/[json, os, strformat, strutils]

{.compile: "nyx.c".}
{.compile: "atropos_request_shm.c".}

proc nyx_init(): void {.importc.}
proc nyx_create_snapshot(): void {.importc.}
proc nyx_exit(): void {.importc.}
proc nyx_get_payload(): cstring {.importc.}
proc nyx_get_shm_id(): cint {.importc.}
proc nyx_get_bitmap_size(): uint32 {.importc.}
proc nyx_report_crash(message: cstring): void {.importc.}
proc nyx_hprintf(message: cstring): void {.importc.}
proc nyx_coverage_dump(buffer: cstring, length: uint32, cpu: uint32, kind: uint8): void {.importc.}

proc atropos_request_channel_create(): cint {.importc.}
proc atropos_request_channel_state(): cint {.importc.}
proc atropos_request_channel_publish(payload: cstring, length: uint32, sequence: uint32): cint {.importc.}
proc atropos_request_channel_wait_done(timeout_ms: uint32): cint {.importc.}
proc atropos_request_channel_output(): cstring {.importc.}
proc atropos_request_channel_output_length(): uint32 {.importc.}
proc atropos_request_channel_flags(): uint32 {.importc.}
proc nyx_capture_coverage_baseline(): cint {.importc.}
proc nyx_apply_coverage_baseline(): void {.importc.}

const
  ChannelBootReady = 1
  ChannelOutputTruncated = 1'u32
  MaxRequestBytes = 1024 * 1024
  MaxWaitForWordPressMs = 300_000'u32
  RequestWaitMs = 120_000'u32
  SecretReadTrigger = "secret4815162342"

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

proc startMariaDb() =
  discard execCmd("chown -R mysql:mysql /var/lib/mysql /var/run/mysqld; service mysql restart")
  discard execCmd("chmod -R 777 /var/lib/php/sessions/")
  sleep(3000)

proc startPhpWorker(shmId: cint, bitmapSize: uint32) =
  discard execCmd("rm -f /tmp/redqueen_mode_enabled /tmp/coverage_dump_enabled /tmp/execution_limit /tmp/bug_triggered /tmp/atropos-php-coverage-enabled")
  discard execCmd("rm -rf /tmp/atropos-php-coverage")
  let launch = fmt"IN_NYX=1 SHM_ID={nyx_get_shm_id()} BITMAP_SIZE={bitmapSize} " &
    fmt"ATROPOS_REQUEST_SHM_ID={shmId} SERVER_NAME=localhost REDIRECT_STATUS=1 " &
    "NYX_REPORT_LFI=1 NYX_INCLUDE_ERROR_IS_LFI=1 NYX_REPORT_EVAL=1 " &
    "NYX_REPORT_SQL_INJECTION=1 NYX_REPORT_UNSERIALIZE=1 " &
    "LD_LIBRARY_PATH=/tmp/ LD_BIND_NOW=1 " &
    "/tmp/php-cli -c /tmp/php.ini /var/www/html/index.php >/tmp/php-cli.log 2>&1 &"
  discard execCmd(launch)

proc waitForWordPressCheckpoint(): bool =
  for attempt in 0 ..< int(MaxWaitForWordPressMs div 100):
    if atropos_request_channel_state() == ChannelBootReady:
      return true
    sleep(100)
  false

proc freshPayload(): string =
  let raw = nyx_get_payload()
  if raw == nil:
    return ""
  result = newString(MaxRequestBytes)
  var length = 0
  while length < MaxRequestBytes and raw[length] != '\0':
    inc length
  result.setLen(length)

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

proc copyRequestOutput(): string =
  let length = int(atropos_request_channel_output_length())
  if length <= 0:
    return ""
  let output = atropos_request_channel_output()
  if output == nil:
    return ""
  result = newString(length)
  copyMem(addr result[0], output, length)

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
  let requestShmId = atropos_request_channel_create()
  if requestShmId < 0:
    guestLog("Could not create the Atropos request shared-memory segment\n")
    quit(1)

  guestLog(fmt"Created Atropos request shared-memory segment {requestShmId}\n")
  guestLog("Restarting MariaDB before WordPress bootstrap\n")
  startMariaDb()
  guestLog("MariaDB restart returned\n")
  startPhpWorker(requestShmId, nyx_get_bitmap_size())
  guestLog("Started PHP CLI request worker; waiting at the WordPress pre-plugin checkpoint\n")
  if not waitForWordPressCheckpoint():
    guestLog("WordPress did not reach the wp-settings checkpoint within five minutes\n")
    if fileExists("/tmp/php-cli.log"):
      guestLog(readFile("/tmp/php-cli.log"))
    quit(1)

  guestLog("WordPress reached the pre-plugin checkpoint; acquiring Nyx snapshot\n")
  if nyx_capture_coverage_baseline() != 0:
    guestLog("Could not preserve the PCOV bootstrap coverage bitmap\n")
  let arbitraryWriteOracleCrcBefore = writeOracleCrc()
  let payload = parsePayloadWithSnapshotHandshake()
  if payload["requests"].len == 0:
    nyx_exit()
    quit(0)

  let payloadText = freshPayload()
  if payloadText.len == 0 or payloadText.len > MaxRequestBytes:
    guestLog("Nyx supplied an empty or oversized HTTP payload\n")
    nyx_exit()
    quit(0)
  let coverageConfig = requestConfig(payload, "COVERAGE_DUMP")
  let detailedCoverage = coverageConfig.len > 0
  let coverageCpu = if detailedCoverage: uint32(parseInt(coverageConfig)) else: 0'u32
  nyx_apply_coverage_baseline()

  discard execCmd("rm -f /tmp/bug_triggered")
  if detailedCoverage:
    discard execCmd("mkdir -p /tmp/atropos-php-coverage; rm -f /tmp/atropos-php-coverage/current.cobertura.xml /tmp/atropos-php-coverage/current.cov /tmp/atropos-php-coverage/error.log")

  if atropos_request_channel_publish(payloadText.cstring, uint32(payloadText.len), 1'u32) != 0:
    guestLog("Could not publish the request to PHP shared memory\n")
    nyx_exit()
    quit(0)

  let waitResult = atropos_request_channel_wait_done(RequestWaitMs)
  if waitResult != 0:
    guestLog(fmt"PHP request did not finish through shared memory (status {waitResult})\n")
    if fileExists("/tmp/php-cli.log"):
      guestLog(readFile("/tmp/php-cli.log"))
    nyx_exit()
    quit(0)

  let output = copyRequestOutput()
  var crashLog = ""
  if SecretReadTrigger in output:
    crashLog.add("bug oracle triggered: validated arbitrary read in /var/www/html/index.php\n")
  if (atropos_request_channel_flags() and ChannelOutputTruncated) != 0'u32:
    guestLog("PHP response exceeded the shared-memory response capacity and was truncated\n")
  let arbitraryWriteOracleCrcAfter = writeOracleCrc()
  if arbitraryWriteOracleCrcBefore != arbitraryWriteOracleCrcAfter:
    crashLog.add("bug oracle triggered: validated arbitrary file write/delete/rename\n")

  if detailedCoverage:
    dumpCoverage(coverageCpu)
  reportCrashes(crashLog)
  nyx_exit()

when isMainModule:
  main()
