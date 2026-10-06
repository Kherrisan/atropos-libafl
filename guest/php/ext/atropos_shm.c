#ifdef HAVE_CONFIG_H
# include "config.h"
#endif

#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <unistd.h>

#include "php.h"
#include "atropos_shared.h"

static struct atropos_shared_memory *atropos_shared = NULL;
static int atropos_shared_error = 0;

static int atropos_attach_shared_memory(void) {
	if (atropos_shared) {
		return SUCCESS;
	}
	const char *id_text = getenv("ATROPOS_REQUEST_SHM_ID");
	if (!id_text || !*id_text) {
		atropos_shared_error = EINVAL;
		return FAILURE;
	}
	char *end = NULL;
	long id = strtol(id_text, &end, 10);
	if (!end || *end || id < 0 || id > 0x7fffffff) {
		atropos_shared_error = EINVAL;
		return FAILURE;
	}
	void *memory = shmat((int)id, NULL, 0);
	if (memory == (void *)-1) {
		atropos_shared_error = errno;
		return FAILURE;
	}
	atropos_shared = (struct atropos_shared_memory *)memory;
	if (atropos_shared->magic != ATROPOS_SHARED_MAGIC ||
	    atropos_shared->version != ATROPOS_SHARED_VERSION) {
		atropos_shared = NULL;
		atropos_shared_error = EPROTO;
		return FAILURE;
	}
	return SUCCESS;
}

PHP_FUNCTION(atropos_request_wait) {
	ZEND_PARSE_PARAMETERS_START(0, 0)
	ZEND_PARSE_PARAMETERS_END();

	if (atropos_attach_shared_memory() != SUCCESS) {
		php_error_docref(NULL, E_WARNING, "cannot attach Atropos request shared memory (errno %d)", atropos_shared_error);
		RETURN_FALSE;
	}

	for (;;) {
		uint32_t state = atropos_shared_load_state(atropos_shared);
		if (state == ATROPOS_SHARED_STARTING) {
			uint32_t expected = ATROPOS_SHARED_STARTING;
			__atomic_compare_exchange_n(&atropos_shared->state, &expected,
			                            ATROPOS_SHARED_BOOT_READY, 0,
			                            __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
			usleep(1000);
			continue;
		}
		if (state == ATROPOS_SHARED_REQUEST_READY) {
			uint32_t expected = ATROPOS_SHARED_REQUEST_READY;
			if (!__atomic_compare_exchange_n(&atropos_shared->state, &expected,
			                                 ATROPOS_SHARED_REQUEST_RUNNING, 0,
			                                 __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
				continue;
			}
			if (atropos_shared->input_length == 0 ||
			    atropos_shared->input_length > ATROPOS_SHARED_INPUT_CAPACITY) {
				atropos_shared_store_state(atropos_shared, ATROPOS_SHARED_REQUEST_FAILED);
				RETURN_FALSE;
			}
			RETURN_STRINGL(atropos_shared->input, atropos_shared->input_length);
		}
		if (state == ATROPOS_SHARED_REQUEST_FAILED) {
			RETURN_FALSE;
		}
		usleep(1000);
	}
}

PHP_FUNCTION(atropos_request_complete) {
	char *output = NULL;
	size_t output_length = 0;
	zend_long http_status = 200;

	ZEND_PARSE_PARAMETERS_START(2, 2)
		Z_PARAM_STRING(output, output_length)
		Z_PARAM_LONG(http_status)
	ZEND_PARSE_PARAMETERS_END();

	if (atropos_attach_shared_memory() != SUCCESS ||
	    atropos_shared_load_state(atropos_shared) != ATROPOS_SHARED_REQUEST_RUNNING) {
		RETURN_FALSE;
	}
	if (http_status < 100 || http_status > 599) {
		http_status = 200;
	}
	size_t copied = output_length;
	atropos_shared->flags = 0;
	if (copied > ATROPOS_SHARED_OUTPUT_CAPACITY) {
		copied = ATROPOS_SHARED_OUTPUT_CAPACITY;
		atropos_shared->flags |= ATROPOS_SHARED_OUTPUT_TRUNCATED;
	}
	if (copied > 0) {
		memcpy(atropos_shared->output, output, copied);
	}
	atropos_shared->output_length = (uint32_t)copied;
	atropos_shared->http_status = (int32_t)http_status;
	atropos_shared_store_state(atropos_shared, ATROPOS_SHARED_REQUEST_DONE);
	RETURN_TRUE;
}

static const zend_function_entry atropos_shm_functions[] = {
	PHP_FE(atropos_request_wait, NULL)
	PHP_FE(atropos_request_complete, NULL)
	PHP_FE_END
};

zend_module_entry atropos_shm_module_entry = {
	STANDARD_MODULE_HEADER,
	"atropos_shm",
	atropos_shm_functions,
	NULL,
	NULL,
	NULL,
	NULL,
	NULL,
	"1.0.0",
	STANDARD_MODULE_PROPERTIES
};

#ifdef COMPILE_DL_ATROPOS_SHM
# ifdef ZTS
ZEND_TSRMLS_CACHE_DEFINE()
# endif
ZEND_GET_MODULE(atropos_shm)
#endif
