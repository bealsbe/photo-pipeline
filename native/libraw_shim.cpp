// Thin C shim over LibRaw.
//
// LibRaw's own C API has no setters for `half_size` or `use_camera_wb`, and
// reaching into libraw_data_t from Rust would mean mirroring a large,
// version-specific struct.  Compiling this file against the vendored headers
// keeps the Rust side down to a handful of plain functions and one small,
// stable image struct.
//
// The calls mirror what rawpy does for the Python app:
//   extract_thumb()  -> libraw_unpack_thumb + libraw_dcraw_make_mem_thumb
//   postprocess(use_camera_wb=True, half_size=..., no_auto_bright=False,
//               output_bps=8)
//                    -> set params, libraw_unpack, libraw_dcraw_process,
//                       libraw_dcraw_make_mem_image

#include "libraw/libraw.h"

extern "C" {

typedef struct {
    int type;              // 1 = JPEG bytes, 2 = bitmap
    int width;
    int height;
    int colors;
    int bits;
    unsigned int size;     // bytes in data
    const unsigned char *data;
    void *owner;           // libraw_processed_image_t*, freed by pp_image_free
} pp_image;

void *pp_open(const char *path, int *err) {
    libraw_data_t *lr = libraw_init(0);
    if (!lr) {
        *err = LIBRAW_UNSPECIFIED_ERROR;
        return 0;
    }
    int e = libraw_open_file(lr, path);
    if (e != LIBRAW_SUCCESS) {
        libraw_close(lr);
        *err = e;
        return 0;
    }
    *err = 0;
    return lr;
}

// LibRaw's orientation code: 0 none, 3 = 180, 5 = 90 CCW, 6 = 90 CW.
int pp_flip(void *h) {
    return ((libraw_data_t *)h)->sizes.flip;
}

static void fill(pp_image *out, libraw_processed_image_t *img) {
    out->type = (int)img->type;
    out->width = img->width;
    out->height = img->height;
    out->colors = img->colors;
    out->bits = img->bits;
    out->size = img->data_size;
    out->data = img->data;
    out->owner = img;
}

int pp_thumb(void *h, pp_image *out) {
    libraw_data_t *lr = (libraw_data_t *)h;
    int e = libraw_unpack_thumb(lr);
    if (e != LIBRAW_SUCCESS)
        return e;
    libraw_processed_image_t *img = libraw_dcraw_make_mem_thumb(lr, &e);
    if (!img)
        return e ? e : LIBRAW_UNSPECIFIED_ERROR;
    fill(out, img);
    return 0;
}

// The smallest embedded JPEG whose long edge is at least `min_edge`.  Newer
// bodies embed a full-size JPEG as well as a ~1616 px preview; the default
// (largest) costs several times more to decode for a 300 px thumbnail.
// Returns an error when none qualifies or it fails to unpack; selecting one
// rewrites LibRaw's thumbnail state, so the caller must reopen the file for
// the default thumbnail rather than calling pp_thumb on this handle.
int pp_thumb_min(void *h, int min_edge, pp_image *out) {
    libraw_data_t *lr = (libraw_data_t *)h;
    const libraw_thumbnail_list_t &tl = lr->thumbs_list;
    int best = -1, best_edge = 0;
    for (int i = 0; i < tl.thumbcount && i < LIBRAW_THUMBNAIL_MAXCOUNT; i++) {
        const libraw_thumbnail_item_t &t = tl.thumblist[i];
        if (t.tformat != LIBRAW_INTERNAL_THUMBNAIL_JPEG || t.tlength == 0)
            continue;
        int edge = t.twidth > t.theight ? t.twidth : t.theight;
        if (edge < min_edge)
            continue;
        if (best < 0 || edge < best_edge) {
            best = i;
            best_edge = edge;
        }
    }
    if (best < 0)
        return LIBRAW_REQUEST_FOR_NONEXISTENT_THUMBNAIL;
    int e = libraw_unpack_thumb_ex(lr, best);
    if (e != LIBRAW_SUCCESS)
        return e;
    libraw_processed_image_t *img = libraw_dcraw_make_mem_thumb(lr, &e);
    if (!img)
        return e ? e : LIBRAW_UNSPECIFIED_ERROR;
    fill(out, img);
    return 0;
}

// Size of the default (largest) embedded thumbnail, known after open.
int pp_thumb_size(void *h, int *w, int *hgt) {
    libraw_data_t *lr = (libraw_data_t *)h;
    *w = lr->thumbnail.twidth;
    *hgt = lr->thumbnail.theight;
    return (*w > 0 && *hgt > 0) ? 0 : LIBRAW_NO_THUMBNAIL;
}

int pp_process(void *h, int half_size, pp_image *out) {
    libraw_data_t *lr = (libraw_data_t *)h;
    lr->params.half_size = half_size;
    lr->params.use_camera_wb = 1;
    lr->params.no_auto_bright = 0;
    lr->params.output_bps = 8;
    int e = libraw_unpack(lr);
    if (e != LIBRAW_SUCCESS)
        return e;
    e = libraw_dcraw_process(lr);
    if (e != LIBRAW_SUCCESS)
        return e;
    libraw_processed_image_t *img = libraw_dcraw_make_mem_image(lr, &e);
    if (!img)
        return e ? e : LIBRAW_UNSPECIFIED_ERROR;
    fill(out, img);
    return 0;
}

void pp_image_free(pp_image *img) {
    if (img && img->owner) {
        libraw_dcraw_clear_mem((libraw_processed_image_t *)img->owner);
        img->owner = 0;
        img->data = 0;
    }
}

void pp_close(void *h) {
    if (h)
        libraw_close((libraw_data_t *)h);
}

const char *pp_strerror(int e) {
    return libraw_strerror(e);
}

} // extern "C"
