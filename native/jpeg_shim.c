/* JPEG decoding through the system libjpeg-turbo — the same SIMD decoder Qt
 * uses, so the viewer and thumbnails are at least as fast as the Python app.
 *
 * One entry point, called twice: first with buf == NULL to learn the output
 * size for a given DCT scale, then with a buffer of that size.  Errors
 * longjmp back here (libjpeg's convention), which is why this file is C.
 */

#include <setjmp.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>

#include <jpeglib.h>

struct pp_jpeg_error {
    struct jpeg_error_mgr pub;
    jmp_buf jump;
};

static void pp_error_exit(j_common_ptr cinfo) {
    struct pp_jpeg_error *err = (struct pp_jpeg_error *)cinfo->err;
    longjmp(err->jump, 1);
}

/* Corrupt-data warnings are not worth a line on stderr per file. */
static void pp_output_message(j_common_ptr cinfo) {
    (void)cinfo;
}

/* scale_denom: 1, 2, 4 or 8.  mode: 1 for 4-channel RGBA output; 2 for
 * luma only (1 channel: the Y plane of a YCbCr JPEG, so the colour planes
 * are neither transformed nor upsampled); otherwise grayscale stays 1
 * channel and everything else is RGB.  CMYK is never luma-decoded (mode 2
 * gives RGB for it, which the caller converts).
 * Returns 0 on success, 1 on a decode error, 2 if buf is too small. */
int pp_jpeg_decode(const unsigned char *data, unsigned long len, int scale_denom, int mode,
                   unsigned char *buf, size_t buf_len, int *out_w, int *out_h, int *out_c) {
    int rgba = mode == 1;
    int luma = mode == 2;
    struct jpeg_decompress_struct cinfo;
    struct pp_jpeg_error jerr;
    unsigned char *volatile cmyk_row = NULL;

    cinfo.err = jpeg_std_error(&jerr.pub);
    jerr.pub.error_exit = pp_error_exit;
    jerr.pub.output_message = pp_output_message;
    if (setjmp(jerr.jump)) {
        jpeg_destroy_decompress(&cinfo);
        free(cmyk_row);
        return 1;
    }
    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, len);
    if (jpeg_read_header(&cinfo, TRUE) != JPEG_HEADER_OK) {
        jpeg_destroy_decompress(&cinfo);
        return 1;
    }

    int cmyk = cinfo.jpeg_color_space == JCS_CMYK || cinfo.jpeg_color_space == JCS_YCCK;
    int channels;
    if (cmyk) {
        cinfo.out_color_space = JCS_CMYK;
        channels = rgba ? 4 : 3;
    } else if (rgba) {
        cinfo.out_color_space = JCS_EXT_RGBA;
        channels = 4;
    } else if (cinfo.jpeg_color_space == JCS_GRAYSCALE || (luma && (cinfo.jpeg_color_space == JCS_YCbCr || cinfo.jpeg_color_space == JCS_RGB))) {
        cinfo.out_color_space = JCS_GRAYSCALE;
        channels = 1;
    } else {
        cinfo.out_color_space = JCS_RGB;
        channels = 3;
    }
    cinfo.scale_num = 1;
    cinfo.scale_denom = (unsigned int)(scale_denom < 1 ? 1 : scale_denom);
    jpeg_calc_output_dimensions(&cinfo);
    *out_w = (int)cinfo.output_width;
    *out_h = (int)cinfo.output_height;
    *out_c = channels;
    if (!buf) {
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    size_t stride = (size_t)cinfo.output_width * (size_t)channels;
    if (buf_len < stride * cinfo.output_height) {
        jpeg_destroy_decompress(&cinfo);
        return 2;
    }

    jpeg_start_decompress(&cinfo);
    if (cmyk) {
        cmyk_row = (unsigned char *)malloc((size_t)cinfo.output_width * 4);
        if (!cmyk_row) {
            jpeg_destroy_decompress(&cinfo);
            return 1;
        }
        while (cinfo.output_scanline < cinfo.output_height) {
            unsigned char *dst = buf + stride * cinfo.output_scanline;
            JSAMPROW row = cmyk_row;
            jpeg_read_scanlines(&cinfo, &row, 1);
            /* Qt's conversion, which assumes Adobe's inverted CMYK. */
            for (JDIMENSION x = 0; x < cinfo.output_width; x++) {
                const unsigned char *p = cmyk_row + x * 4;
                unsigned int k = p[3];
                dst[0] = (unsigned char)(p[0] * k / 255);
                dst[1] = (unsigned char)(p[1] * k / 255);
                dst[2] = (unsigned char)(p[2] * k / 255);
                if (rgba)
                    dst[3] = 255;
                dst += channels;
            }
        }
    } else {
        JSAMPROW rows[16];
        while (cinfo.output_scanline < cinfo.output_height) {
            JDIMENSION left = cinfo.output_height - cinfo.output_scanline;
            JDIMENSION n = left < 16 ? left : 16;
            for (JDIMENSION i = 0; i < n; i++)
                rows[i] = buf + stride * (cinfo.output_scanline + i);
            jpeg_read_scanlines(&cinfo, rows, n);
        }
    }
    jpeg_finish_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    free(cmyk_row);
    return 0;
}
