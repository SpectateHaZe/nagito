#pragma once
#include <stddef.h>
#include <stdint.h>

typedef struct Vsr Vsr;

// Returns NULL and fills err when RTX Video Super Resolution is unavailable.
Vsr *vsr_create(int in_w, int in_h, int out_w, int out_h, char *err, size_t err_size);
int vsr_size_matches(const Vsr *v, int in_w, int in_h, int out_w, int out_h);
// nv12: in_w*in_h luma then interleaved chroma. rgba: out_w*out_h*4 bytes. Returns 0 on success.
int vsr_process(Vsr *v, const uint8_t *nv12, int full_range, int bt601, uint8_t *rgba);
void vsr_free(Vsr *v);
