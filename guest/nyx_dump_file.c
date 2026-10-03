#include "nyx.h"

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
