// Copyright (c) 2025 BitBank Software, Inc. and Unexpected Maker
// SPDX-License-Identifier: MIT
//
// ESP32-S3 SIMD optimized alpha blend constant color over RGB565 big-endian background.

	.text
	.align 4
	.type   s3_alpha_blend_color_be, @function
	.align 4
	.global s3_alpha_blend_color_be

// Call as:
// void s3_alpha_blend_color_be(uint16_t fgColor, uint16_t *pBG, uint16_t *pDest, uint32_t count, uint8_t alpha, const uint16_t *pMasks);
//   a2: fgColor (native RGB565)
//   a3: pBG (big-endian RGB565)
//   a4: pDest (big-endian RGB565)
//   a5: count (multiple of 8)
//   a6: alpha (0..32)
//   a7: pMasks ([0x001F, 0x07E0, 0x07C0, 0xF800])
s3_alpha_blend_color_be:
    entry    a1, 16

    // make the alpha value as a pair of 16-bit values for later
    slli a8, a6, 16
    or a8, a8, a6                 // now we have the alpha in 16-bit slots

    ee.movi.32.q q5, a8, 0        // set up alpha value in 8 slots of Q5
    ee.movi.32.q q5, a8, 1
    ee.movi.32.q q5, a8, 2
    ee.movi.32.q q5, a8, 3

    movi.n    a8, 32              // inverted alpha for destination
    sub a6, a8, a6
    slli a8, a6, 16               // prepare it for 16-bit slots
    or a8, a8, a6
    ee.movi.32.q q6, a8, 0        // set up inverted alpha in 8 slots of Q6
    ee.movi.32.q q6, a8, 1
    ee.movi.32.q q6, a8, 2
    ee.movi.32.q q6, a8, 3

    movi.n a8, 0                  // prepare shift amounts
    movi.n a9, 5

    // prepare 32-bit broadcast of native fgColor
    extui   a12, a2, 0, 16
    slli    a13, a12, 16
    or      a12, a12, a13

.alpha_loop_color_be:
    wsr.sar    a8                 // SAR = 0

    ee.movi.32.q q1, a12, 0
    ee.movi.32.q q1, a12, 1
    ee.movi.32.q q1, a12, 2
    ee.movi.32.q q1, a12, 3

    ee.vld.128.ip    q2, a3, 16   // load 8 RGB565 dest pixels from big-endian memory

    // swap dest pixels byte order to native
    mv.qr q7, q2
    ee.vunzip.8 q7, q2
    ee.vzip.8 q2, q7

    // Blue channel
    ee.vldbc.16.ip q0, a7, 2      // load blue mask (0x001F) into all 8 slots
    ee.andq q3, q1, q0            // apply mask
    ee.andq q4, q2, q0
    ee.vmul.u16 q3, q3, q5        // multiply source by alpha
    ee.vmul.u16 q4, q4, q6        // multiply dest by (32 - alpha)
    ee.vadds.s16 q7, q3, q4       // combine src + dest blue
    wsr.sar a9                    // SAR = 5
    ee.vsr.32 q7, q7              // shift blue back to normal position
    ee.andq q7, q7, q0

    // Green channel
    ee.vldbc.16.ip q0, a7, 2      // load green mask (0x07E0)
    ee.andq q3, q1, q0
    ee.andq q4, q2, q0
    ee.vmul.u16 q3, q3, q5        // mult green by src alpha (SAR = 5)
    ee.vmul.u16 q4, q4, q6        // mult green by dest alpha (SAR = 5)
    ee.vadds.s16 q4, q3, q4       // combine src + dest green
    ee.andq q4, q4, q0
    ee.orq q7, q7, q4             // combine green + blue

    // Red channel
    ee.vldbc.16.ip q0, a7, 2      // load red shifted mask (0x07C0)
    ee.vsr.32 q1, q1              // shift red down 5 bits to prevent overflow
    ee.vsr.32 q2, q2
    ee.andq q1, q1, q0            // mask at new position
    ee.andq q2, q2, q0
    ee.vmul.u16 q1, q1, q5        // mult red by src alpha (SAR = 5)
    ee.vmul.u16 q2, q2, q6        // mult red by dest alpha (SAR = 5)
    ee.vadds.s16 q1, q1, q2       // combine src + dest red
    ee.vsl.32 q1, q1              // shift left 5 to restore red to bits 15..11
    ee.vldbc.16 q0, a7            // load original red mask (0xF800)
    ee.andq q1, q1, q0
    ee.orq q1, q7, q1             // combine R + G + B

    // Reverse byte order back to big-endian for storage
    mv.qr q7, q1
    ee.vunzip.8 q7, q1
    ee.vzip.8 q1, q7

    ee.vst.128.ip q1, a4, 16      // store 8 finished pixels
    addi.n a7, a7, -6             // reset color masks pointer
    addi.n a5, a5, -8             // decrement pixel count by 8
    bnez.n a5, .alpha_loop_color_be

    retw.n
