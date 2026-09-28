/* Embed SAPI harness. php_execute_script() catches die()/zend_bailout. */
#include <php_embed.h>

#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define RESPONSE_CAP (1024 * 1024)
#define BITMAP_SIZE 8388608
#define SCRIPT_PATH "/home/user/wordpress/index.php"
#define CRASH_FILE "/home/user/wordpress/crash.php"

struct atropos_shared {
	uint32_t response_len;
	uint32_t crc_before;
	uint32_t crc_after;
	uint32_t flags;
	char response[RESPONSE_CAP];
};

static struct atropos_shared *shared_state;

static const char *post_data;
static size_t post_len;
static size_t post_off;

static const char *cookie_header;
static const char *extra_headers;
static const char *req_method;
static const char *req_uri;
static const char *req_query;
static const char *content_type;
static char content_length_buf[32];
static char exec_limit_buf[32];
static int redqueen_on;
static uint32_t exec_limit;

static uint32_t crc32_update(uint32_t crc, const unsigned char *data, size_t len) {
	size_t i;
	int bit;
	crc = ~crc;
	for (i = 0; i < len; i++) {
		crc ^= data[i];
		for (bit = 0; bit < 8; bit++) {
			uint32_t mask = -(crc & 1u);
			crc = (crc >> 1) ^ (0xEDB88320u & mask);
		}
	}
	return ~crc;
}

static uint32_t file_crc(const char *path) {
	FILE *handle = fopen(path, "rb");
	unsigned char buf[4096];
	uint32_t crc = 0;
	size_t n;
	if (handle == NULL) {
		return 0;
	}
	while ((n = fread(buf, 1, sizeof(buf), handle)) > 0) {
		crc = crc32_update(crc, buf, n);
	}
	fclose(handle);
	return crc;
}

static int ensure_shm(const char *name, size_t size, void **out) {
	int fd = shm_open(name, O_RDWR | O_CREAT, 0666);
	void *map;
	if (fd < 0) {
		perror(name);
		return -1;
	}
	if (ftruncate(fd, (off_t)size) != 0) {
		perror("ftruncate");
		close(fd);
		return -1;
	}
	map = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	close(fd);
	if (map == MAP_FAILED) {
		perror("mmap");
		return -1;
	}
	*out = map;
	return 0;
}

static size_t atropos_ub_write(const char *str, size_t len) {
	size_t room;
	size_t n;
	if (shared_state == NULL) {
		return len;
	}
	room = RESPONSE_CAP - shared_state->response_len;
	n = len < room ? len : room;
	if (n > 0) {
		memcpy(shared_state->response + shared_state->response_len, str, n);
		shared_state->response_len += (uint32_t)n;
	}
	return len;
}

static void atropos_flush(void *server_context) {
	(void)server_context;
}

static size_t atropos_read_post(char *buffer, size_t count_bytes) {
	size_t left = post_len - post_off;
	if (count_bytes > left) {
		count_bytes = left;
	}
	if (count_bytes == 0) {
		return 0;
	}
	memcpy(buffer, post_data + post_off, count_bytes);
	post_off += count_bytes;
	return count_bytes;
}

static char *atropos_read_cookies(void) {
	if (cookie_header == NULL || cookie_header[0] == '\0') {
		return NULL;
	}
	return estrdup(cookie_header);
}

static void atropos_register_vars(zval *track_vars_array) {
	php_import_environment_variables(track_vars_array);
	php_register_variable("REQUEST_METHOD", req_method != NULL ? req_method : "GET", track_vars_array);
	php_register_variable("REQUEST_URI", req_uri != NULL ? req_uri : "/", track_vars_array);
	php_register_variable("QUERY_STRING", req_query != NULL ? req_query : "", track_vars_array);
	php_register_variable("SCRIPT_NAME", "/index.php", track_vars_array);
	php_register_variable("PHP_SELF", "/index.php", track_vars_array);
	php_register_variable("SCRIPT_FILENAME", SCRIPT_PATH, track_vars_array);
	php_register_variable("DOCUMENT_ROOT", "/home/user/wordpress", track_vars_array);
	php_register_variable("HTTP_HOST", "127.0.0.1", track_vars_array);
	php_register_variable("SERVER_NAME", "127.0.0.1", track_vars_array);
	php_register_variable("SERVER_PORT", "80", track_vars_array);
	php_register_variable("CONTENT_TYPE", content_type != NULL ? content_type : "", track_vars_array);
	php_register_variable("CONTENT_LENGTH", content_length_buf, track_vars_array);
	php_register_variable("HTTP_X_ATROPOS_ID", "1", track_vars_array);
	if (redqueen_on) {
		php_register_variable("HTTP_X_ATROPOS_REDQUEEN", "0", track_vars_array);
	}
	if (exec_limit > 0) {
		php_register_variable("HTTP_X_ATROPOS_EXEC_LIMIT", exec_limit_buf, track_vars_array);
	}
	if (extra_headers != NULL && extra_headers[0] != '\0') {
		char *copy = estrdup(extra_headers);
		char *line = copy;
		while (line != NULL && *line != '\0') {
			char *next = strchr(line, '\n');
			char *colon;
			char name[160];
			char *cursor;
			if (next != NULL) {
				*next = '\0';
				next++;
			}
			colon = strchr(line, ':');
			if (colon != NULL && colon != line) {
				*colon = '\0';
				snprintf(name, sizeof(name), "HTTP_%s", line);
				for (cursor = name; *cursor != '\0'; cursor++) {
					if (*cursor == '-') {
						*cursor = '_';
					}
					*cursor = (char)toupper((unsigned char)*cursor);
				}
				while (*(colon + 1) == ' ') {
					colon++;
				}
				php_register_variable(name, colon + 1, track_vars_array);
			}
			line = next;
		}
		efree(copy);
	}
}

static void set_request_defaults(void) {
	req_method = "GET";
	req_uri = "/";
	req_query = "";
	content_type = "";
	cookie_header = NULL;
	post_data = "";
	post_len = 0;
	post_off = 0;
	redqueen_on = 0;
	exec_limit = 0;
	snprintf(content_length_buf, sizeof(content_length_buf), "0");
}

int atropos_boot(void) {
	static char arg0[] = "atropos-libafl";
	static char *argv[] = {arg0, NULL};
	void *bitmap = NULL;

	set_request_defaults();
	setenv("ATROPOS_BITMAP", "/atropos_bitmap", 1);
	setenv("BITMAP_SIZE", "8388608", 1);
	setenv("PHPRC", "/opt/atropos-embed/lib", 1);
	mkdir("/dev/shm/atropos", 0777);

	if (ensure_shm("/atropos_bitmap", BITMAP_SIZE, &bitmap) != 0) {
		return -1;
	}
	if (ensure_shm("/atropos_response", sizeof(*shared_state), (void **)&shared_state) != 0) {
		return -1;
	}
	memset(shared_state, 0, sizeof(*shared_state));

	php_embed_module.ub_write = atropos_ub_write;
	php_embed_module.flush = atropos_flush;
	php_embed_module.read_post = atropos_read_post;
	php_embed_module.read_cookies = atropos_read_cookies;
	php_embed_module.register_server_variables = atropos_register_vars;

	if (php_embed_init(1, argv) != SUCCESS) {
		fprintf(stderr, "php_embed_init failed\n");
		return -1;
	}
	/* Drop the request embed_init opens. The engine stays up for later requests. */
	php_request_shutdown(NULL);
	return 0;
}

int atropos_execute(
	const char *method,
	const char *uri,
	const char *query,
	const char *type,
	const char *body,
	size_t body_len,
	const char *cookie,
	const char *headers,
	int redqueen,
	uint32_t limit
) {
	zend_file_handle file_handle;
	int rc;

	req_method = (method != NULL && method[0] != '\0') ? method : "GET";
	req_uri = (uri != NULL && uri[0] != '\0') ? uri : "/";
	req_query = query != NULL ? query : "";
	content_type = type != NULL ? type : "";
	cookie_header = cookie;
	extra_headers = headers;
	post_data = body != NULL ? body : "";
	post_len = body_len;
	post_off = 0;
	redqueen_on = redqueen;
	exec_limit = limit;
	snprintf(content_length_buf, sizeof(content_length_buf), "%zu", post_len);
	snprintf(exec_limit_buf, sizeof(exec_limit_buf), "%u", limit);

	unlink("/tmp/bug_triggered");
	if (redqueen_on) {
		int fd = open("/dev/shm/atropos/strings_0", O_CREAT | O_TRUNC | O_WRONLY, 0666);
		if (fd >= 0) {
			close(fd);
		}
	}

	if (shared_state != NULL) {
		shared_state->response_len = 0;
		shared_state->crc_before = file_crc(CRASH_FILE);
		shared_state->crc_after = shared_state->crc_before;
		shared_state->flags = 0;
	}

	SG(request_info).argc = 0;
	SG(request_info).argv = NULL;
	SG(request_info).request_method = req_method;
	SG(request_info).query_string = (char *)req_query;
	SG(request_info).path_translated = SCRIPT_PATH;
	SG(request_info).request_uri = (char *)req_uri;
	SG(request_info).content_type = content_type;
	SG(request_info).content_length = (zend_long)post_len;
	SG(request_info).proto_num = 1001;

	if (php_request_startup() == FAILURE) {
		fprintf(stderr, "php_request_startup failed\n");
		return 1;
	}

	zend_stream_init_filename(&file_handle, SCRIPT_PATH);
	rc = php_execute_script(&file_handle);
	zend_destroy_file_handle(&file_handle);
	php_request_shutdown(NULL);

	if (shared_state != NULL) {
		shared_state->crc_after = file_crc(CRASH_FILE);
		shared_state->flags = 1;
	}
	(void)rc;
	return 0;
}

const char *atropos_response_ptr(void) {
	if (shared_state == NULL) {
		return "";
	}
	return shared_state->response;
}

uint32_t atropos_response_len(void) {
	if (shared_state == NULL) {
		return 0;
	}
	return shared_state->response_len;
}

uint32_t atropos_crc_before(void) {
	return shared_state != NULL ? shared_state->crc_before : 0;
}

uint32_t atropos_crc_after(void) {
	return shared_state != NULL ? shared_state->crc_after : 0;
}

#ifdef ATROPOS_STANDALONE
int main(void) {
	static const char body[] =
		"{\"validation\":\"normal\",\"requests\":[{\"method\":\"POST\","
		"\"path\":\"/wp/v2/posts\",\"body\":{\"title\":\"seed\"},\"headers\":{}}]}";
	uint32_t len;
	int i;

	if (atropos_boot() != 0) {
		return 1;
	}
	for (i = 0; i < 2; i++) {
		if (atropos_execute("POST", "/wp-json/batch/v1", "", "application/json",
				body, strlen(body), NULL, NULL, 1, 0) != 0) {
			return 1;
		}
		len = atropos_response_len();
		fprintf(stderr, "STILL_ALIVE len=%u\n", len);
		fwrite(atropos_response_ptr(), 1, len > 800 ? 800 : len, stdout);
		fputc('\n', stdout);
	}
	fprintf(stderr, "BOTH_REQUESTS_RETURNED\n");
	return 0;
}
#endif
