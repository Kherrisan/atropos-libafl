#include <stdlib.h>

#include "nyx.h"

int nyx_get_bitmap_size(void);

static uint8_t *atropos_coverage_baseline = NULL;
static uint32_t atropos_coverage_baseline_size = 0;

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

/*
 * Overwrite {workdir}/dump/<name> with one guest buffer.
 * append=0, so each fuzz execution replaces the previous PHP CLI log.
 */
void nyx_dump_file(const char *name, const void *data, uint32_t len) {
	static kafl_dump_file_t dump_obj;
	static char filename[256];

	if (name == NULL || name[0] == '\0') {
		return;
	}

	snprintf(filename, sizeof(filename), "%s", name);
	dump_obj.file_name_str_ptr = (uintptr_t)filename;
	dump_obj.data_ptr = (uintptr_t)data;
	dump_obj.bytes = len;
	dump_obj.append = 0;
	kAFL_hypercall(HYPERCALL_KAFL_DUMP_FILE, (uintptr_t)&dump_obj);
}
