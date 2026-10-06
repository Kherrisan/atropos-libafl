#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <unistd.h>

#include "atropos_shared.h"
#include "nyx.h"

static int atropos_shared_id = -1;
static struct atropos_shared_memory *atropos_shared = NULL;
static uint8_t *atropos_coverage_baseline = NULL;
static uint32_t atropos_coverage_baseline_size = 0;

int atropos_request_channel_create(void) {
	atropos_shared_id = shmget(IPC_PRIVATE, sizeof(*atropos_shared), IPC_CREAT | IPC_EXCL | 0600);
	if (atropos_shared_id < 0) {
		return -1;
	}
	atropos_shared = (struct atropos_shared_memory *)shmat(atropos_shared_id, NULL, 0);
	if (atropos_shared == (void *)-1) {
		atropos_shared = NULL;
		shmctl(atropos_shared_id, IPC_RMID, NULL);
		atropos_shared_id = -1;
		return -1;
	}
	memset(atropos_shared, 0, sizeof(*atropos_shared));
	atropos_shared->magic = ATROPOS_SHARED_MAGIC;
	atropos_shared->version = ATROPOS_SHARED_VERSION;
	atropos_shared_store_state(atropos_shared, ATROPOS_SHARED_STARTING);
	return atropos_shared_id;
}

int atropos_request_channel_id(void) {
	return atropos_shared_id;
}

int atropos_request_channel_state(void) {
	if (!atropos_shared) {
		return -1;
	}
	return (int)atropos_shared_load_state(atropos_shared);
}

int atropos_request_channel_publish(const char *payload, uint32_t length, uint32_t sequence) {
	if (!atropos_shared || !payload || length == 0 || length > ATROPOS_SHARED_INPUT_CAPACITY) {
		return -1;
	}
	memcpy(atropos_shared->input, payload, length);
	atropos_shared->input_length = length;
	atropos_shared->output_length = 0;
	atropos_shared->http_status = 0;
	atropos_shared->flags = 0;
	atropos_shared->sequence = sequence;
	atropos_shared_store_state(atropos_shared, ATROPOS_SHARED_REQUEST_READY);
	return 0;
}

int atropos_request_channel_wait_done(uint32_t timeout_ms) {
	if (!atropos_shared) {
		return -1;
	}
	for (uint32_t elapsed = 0; elapsed < timeout_ms; elapsed += 1) {
		uint32_t state = atropos_shared_load_state(atropos_shared);
		if (state == ATROPOS_SHARED_REQUEST_DONE) {
			return 0;
		}
		if (state == ATROPOS_SHARED_REQUEST_FAILED) {
			return -2;
		}
		usleep(1000);
	}
	return -3;
}

const char *atropos_request_channel_output(void) {
	return atropos_shared ? atropos_shared->output : NULL;
}

uint32_t atropos_request_channel_output_length(void) {
	return atropos_shared ? atropos_shared->output_length : 0;
}

int32_t atropos_request_channel_http_status(void) {
	return atropos_shared ? atropos_shared->http_status : 0;
}

uint32_t atropos_request_channel_flags(void) {
	return atropos_shared ? atropos_shared->flags : 0;
}

int nyx_capture_coverage_baseline(void) {
	if (!trace_buffer) {
		return -1;
	}
	int size = nyx_get_bitmap_size();
	if (size <= 0) {
		return -1;
	}
	free(atropos_coverage_baseline);
	atropos_coverage_baseline = (uint8_t *)malloc((size_t)size);
	if (!atropos_coverage_baseline) {
		atropos_coverage_baseline_size = 0;
		return -1;
	}
	memcpy(atropos_coverage_baseline, trace_buffer, (size_t)size);
	atropos_coverage_baseline_size = (uint32_t)size;
	return 0;
}

void nyx_apply_coverage_baseline(void) {
	if (!trace_buffer || !atropos_coverage_baseline || atropos_coverage_baseline_size == 0) {
		return;
	}
	for (uint32_t i = 0; i < atropos_coverage_baseline_size; ++i) {
		trace_buffer[i] |= atropos_coverage_baseline[i];
	}
}

void nyx_coverage_dump(char *buffer, uint32_t length, uint32_t pinned_core, uint8_t kind) {
	static int initialized[4] = {0, 0, 0, 0};
	static char filenames[4][64];
	static kafl_dump_file_t files[4] = {{0}};

	if (kind > 3 || length == 0 || !buffer) {
		return;
	}
	if (!initialized[kind]) {
		snprintf(filenames[kind], sizeof(filenames[kind]),
		         kind == 0 ? "coverage_cobertura_%u" :
		         kind == 1 ? "coverage_php_%u" :
	         kind == 2 ? "coverage_baseline_cobertura_%u" :
	                     "coverage_baseline_php_%u", pinned_core);
		files[kind].file_name_str_ptr = (uintptr_t)filenames[kind];
		files[kind].append = 0;
		kAFL_hypercall(HYPERCALL_KAFL_DUMP_FILE, (uintptr_t)&files[kind]);
		initialized[kind] = 1;
	}
	files[kind].append = 1;
	files[kind].bytes = length;
	files[kind].data_ptr = (uintptr_t)buffer;
	kAFL_hypercall(HYPERCALL_KAFL_DUMP_FILE, (uintptr_t)&files[kind]);
}
