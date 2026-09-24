// Copyright (c) 2025 BitBank Software, Inc. and Unexpected Maker
// SPDX-License-Identifier: MIT
//
// ESP32-S3 SIMD optimized 16-bit byte-swap copy.

	.text
	.align 4
	.type   s3_simd_bswap16, @function
	.align 4
	.global s3_simd_bswap16

// Call as:
// void s3_simd_bswap16(const uint16_t *src, uint16_t *dst, uint32_t count);
//   a2: src (16-byte aligned)
//   a3: dst (16-byte aligned)
//   a4: count (multiple of 8)
s3_simd_bswap16:
    entry    a1, 16

.bswap16_loop:
    ee.vld.128.ip    q0, a2, 16   // load 8 pixels (16 bytes) into q0 and increment src
    mv.qr            q1, q0
    ee.vunzip.8      q1, q0        // unzip even and odd bytes
    ee.vzip.8        q0, q1        // zip back in opposite order -> swaps bytes in each 16-bit word
    ee.vst.128.ip    q0, a3, 16   // store 8 pixels (16 bytes) to dst and increment dst
    addi.n           a4, a4, -8
    bnez.n           a4, .bswap16_loop

    retw.n
