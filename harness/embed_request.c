/* Run one WordPress request inside the embed SAPI.
 * php_execute_script() catches die()/zend_bailout and returns here.
 */
#include <php_embed.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define RESPONSE_CAP (2 * 1024 * 1024)
#define BITMAP_SIZE 8388608

static char response_buf[RESPONSE_CAP];
static size_t response_len;

static const char *post_data;
static size_t post_len;
static size_t post_off;

static const char *cookie_header;

static const char *req_method;
static const char *req_uri;
static const char *req_query;
static const char *content_type;
static char content_length_buf[32];

static size_t atropos_ub_write(const char *str, size_t len) {
	size_t room = RESPONSE_CAP - 1 - response_len;
	size_t n = len < room ? len : room;
	if (n > 0) {
		memcpy(response_buf + response_len, str, n);
		response_len += n;
		response_buf[response_len] = '\0';
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
	php_register_variable("REQUEST_METHOD", req_method, track_vars_array);
	php_register_variable("REQUEST_URI", req_uri, track_vars_array);
	php_register_variable("QUERY_STRING", req_query != NULL ? req_query : "", track_vars_array);
	php_register_variable("SCRIPT_NAME", "/index.php", track_vars_array);
	php_register_variable("PHP_SELF", "/index.php", track_vars_array);
	php_register_variable("SCRIPT_FILENAME", "/home/user/wordpress/index.php", track_vars_array);
	php_register_variable("DOCUMENT_ROOT", "/home/user/wordpress", track_vars_array);
	php_register_variable("HTTP_HOST", "127.0.0.1", track_vars_array);
	php_register_variable("SERVER_NAME", "127.0.0.1", track_vars_array);
	php_register_variable("SERVER_PORT", "80", track_vars_array);
	php_register_variable("CONTENT_TYPE", content_type, track_vars_array);
	php_register_variable("CONTENT_LENGTH", content_length_buf, track_vars_array);
	php_register_variable("HTTP_X_ATROPOS_ID", "1", track_vars_array);
	php_register_variable("HTTP_X_ATROPOS_REDQUEEN", "1", track_vars_array);
}

static void ensure_bitmap(void) {
	int fd = shm_open("/atropos_bitmap", O_RDWR | O_CREAT, 0666);
	if (fd < 0) {
		perror("shm_open /atropos_bitmap");
		return;
	}
	if (ftruncate(fd, BITMAP_SIZE) != 0) {
		perror("ftruncate bitmap");
	}
	close(fd);
}

static int run_request(void) {
	zend_file_handle file_handle;
	int rc;

	post_off = 0;
	response_len = 0;
	response_buf[0] = '\0';

	SG(request_info).request_method = req_method;
	SG(request_info).query_string = estrdup(req_query != NULL ? req_query : "");
	SG(request_info).path_translated = estrdup("/home/user/wordpress/index.php");
	SG(request_info).request_uri = estrdup(req_uri);
	SG(request_info).content_type = content_type;
	SG(request_info).content_length = (zend_long)post_len;
	SG(request_info).proto_num = 1001;

	if (php_request_startup() == FAILURE) {
		fprintf(stderr, "php_request_startup failed\n");
		return 1;
	}

	zend_stream_init_filename(&file_handle, "/home/user/wordpress/index.php");
	rc = php_execute_script(&file_handle);
	zend_destroy_file_handle(&file_handle);
	php_request_shutdown(NULL);

	fprintf(stderr, "STILL_ALIVE execute_rc=%d response_len=%zu\n", rc, response_len);
	fwrite(response_buf, 1, response_len > 800 ? 800 : response_len, stdout);
	fputc('\n', stdout);
	return 0;
}

int main(void) {
	char *argv[] = {"embed_request", NULL};
	static const char body[] =
		"{\"validation\":\"normal\",\"requests\":[{\"method\":\"POST\","
		"\"path\":\"/wp/v2/posts\",\"body\":{\"title\":\"seed\"},\"headers\":{}}]}";

	req_method = "POST";
	req_uri = "/wp-json/batch/v1";
	req_query = "";
	content_type = "application/json";
	post_data = body;
	post_len = strlen(body);
	snprintf(content_length_buf, sizeof(content_length_buf), "%zu", post_len);
	cookie_header = NULL;

	setenv("ATROPOS_BITMAP", "/atropos_bitmap", 1);
	setenv("BITMAP_SIZE", "8388608", 1);
	ensure_bitmap();

	php_embed_module.ub_write = atropos_ub_write;
	php_embed_module.flush = atropos_flush;
	php_embed_module.read_post = atropos_read_post;
	php_embed_module.read_cookies = atropos_read_cookies;
	php_embed_module.register_server_variables = atropos_register_vars;

	if (php_embed_init(1, argv) != SUCCESS) {
		fprintf(stderr, "php_embed_init failed\n");
		return 1;
	}
	/* embed_init opens a request. Close it so the real one starts clean. */
	php_request_shutdown(NULL);

	if (run_request() != 0) {
		return 1;
	}
	if (run_request() != 0) {
		return 1;
	}

	fprintf(stderr, "BOTH_REQUESTS_RETURNED\n");
	php_module_shutdown();
	sapi_shutdown();
	return 0;
}
