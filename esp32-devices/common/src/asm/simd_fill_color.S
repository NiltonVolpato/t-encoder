// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT
//
// ESP32-S3 PIE SIMD RGB565 color fill.
//
// Fills an array of unsigned 16-bit RGB565 pixels with a constant value using
// 128-bit vector stores. The caller is responsible for ensuring the
// destination pointer is 16-byte aligned; any remaining elements that are not
// a multiple of 8 are handled with scalar stores.

	.text
	.align	4
	.type	simd_fill_color, @function
	.align	4
	.global	simd_fill_color

// int simd_fill_color(uint16_t *a, const uint16_t *val, size_t size);
//   a2: destination pointer (16-byte aligned)
//   a3: pointer to the 16-bit fill value
//   a4: number of elements to fill
//   returns 0 on success
simd_fill_color:
	entry	a1, 16

	extui	a5, a4, 0, 3		// a5 = size % 8 (tail elements)
	srli	a4, a4, 3		// a4 = size / 8 (128-bit blocks)

	ee.vldbc.16.ip	q0, a3, 0	// broadcast 16-bit value into all lanes of q0

	loopnez	a4, .Lsimd_loop
		ee.vst.128.ip	q0, a2, 16	// store 8 pixels (16 bytes) and post-increment dst
.Lsimd_loop:

	l16ui	a6, a3, 0		// load fill value for scalar tail
	loopnez	a5, .Ltail_loop
		s16i	a6, a2, 0
		addi	a2, a2, 2
.Ltail_loop:

	movi.n	a2, 0			// return 0 (success)
	retw.n
