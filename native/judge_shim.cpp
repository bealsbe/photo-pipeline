// The fursuit close-call judge's way into llama.cpp and libmtmd (vendored
// at the commit build.rs pins): load a vision language model and its
// projector on one graphics card, and score one question about two
// images by the log-probabilities of the first answer token.
//
// A small C API, so the Rust side (src/ml/judge_worker.rs) needs no
// bindings generator: every call returns a status and fills plain
// buffers.  Only the judge helper process (`photo-pipeline
// --judge-worker`) calls it; a driver fault takes that process down,
// never the app.
//
// Card only: the model is loaded with every layer offloaded to the one
// device asked for (no other device is given to llama.cpp), the
// projector runs on the same device, and the load reports how many
// layers llama.cpp offloaded so the caller can refuse anything short of
// all of them.  Nothing here falls back to the processor.

#include <cctype>
#include <cmath>
#include <cstdarg>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>
#include <algorithm>
#include <mutex>

#include "llama.h"
#include "ggml-backend.h"
#include "mtmd.h"
#include "mtmd-helper.h"

namespace {

struct LogState {
    std::mutex lock;
    bool echo = false;
    int offloaded = -1;
    int layers = -1;
    std::string last_error;
};

LogState g_log;

void log_cb(enum ggml_log_level level, const char * text, void *) {
    if (!text) {
        return;
    }
    std::lock_guard<std::mutex> g(g_log.lock);
    int a = 0, b = 0;
    const char * p = std::strstr(text, "offloaded ");
    if (p && std::sscanf(p, "offloaded %d/%d layers to GPU", &a, &b) == 2) {
        g_log.offloaded = a;
        g_log.layers = b;
    }
    if (level == GGML_LOG_LEVEL_ERROR) {
        g_log.last_error = text;
        while (!g_log.last_error.empty() && (g_log.last_error.back() == '\n' || g_log.last_error.back() == ' ')) {
            g_log.last_error.pop_back();
        }
    }
    if (g_log.echo || level == GGML_LOG_LEVEL_ERROR) {
        std::fputs(text, stderr);
    }
}

void put(char * buf, size_t len, const std::string & s) {
    if (!buf || len == 0) {
        return;
    }
    size_t n = std::min(len - 1, s.size());
    std::memcpy(buf, s.data(), n);
    buf[n] = 0;
}

std::string lower(std::string s) {
    for (auto & c : s) {
        c = (char) std::tolower((unsigned char) c);
    }
    return s;
}

}  // namespace

extern "C" {

struct pp_judge {
    llama_model * model = nullptr;
    llama_context * ctx = nullptr;
    mtmd_context * mtmd = nullptr;
    int n_batch = 2048;
};

// Start the backends and route their logs (echoed to stderr with `echo`).
void pp_judge_init(int echo) {
    {
        std::lock_guard<std::mutex> g(g_log.lock);
        g_log.echo = echo != 0;
    }
    llama_log_set(log_cb, nullptr);
    mtmd_helper_log_set(log_cb, nullptr);
    llama_backend_init();
}

// The graphics devices llama.cpp sees, one per line: "name\tdescription\ttype\tfree\ttotal"
// (type: gpu or igpu).  Returns the number written.
int pp_judge_devices(char * out, size_t len) {
    std::string s;
    int n = 0;
    for (size_t i = 0; i < ggml_backend_dev_count(); i++) {
        ggml_backend_dev_t d = ggml_backend_dev_get(i);
        auto t = ggml_backend_dev_type(d);
        if (t != GGML_BACKEND_DEVICE_TYPE_GPU && t != GGML_BACKEND_DEVICE_TYPE_IGPU) {
            continue;
        }
        size_t free = 0, total = 0;
        ggml_backend_dev_memory(d, &free, &total);
        s += ggml_backend_dev_name(d);
        s += '\t';
        s += ggml_backend_dev_description(d);
        s += '\t';
        s += t == GGML_BACKEND_DEVICE_TYPE_GPU ? "gpu" : "igpu";
        s += '\t' + std::to_string(free) + '\t' + std::to_string(total) + '\n';
        n++;
    }
    put(out, len, s);
    return n;
}

// Load `model` and `mmproj` on the graphics device named `device` (its
// llama.cpp name, e.g. "Vulkan0"), every layer offloaded.  On success
// `*offloaded` and `*layers` say what llama.cpp offloaded; the caller
// refuses a partial offload.  On failure returns null with `err` filled.
pp_judge * pp_judge_load(const char * model, const char * mmproj, const char * device, int n_ctx, int n_batch, int n_ubatch,
                         int image_tokens, int threads, int * offloaded, int * layers, char * err, size_t err_len) {
    ggml_backend_dev_t dev = nullptr;
    for (size_t i = 0; i < ggml_backend_dev_count(); i++) {
        ggml_backend_dev_t d = ggml_backend_dev_get(i);
        auto t = ggml_backend_dev_type(d);
        if ((t == GGML_BACKEND_DEVICE_TYPE_GPU || t == GGML_BACKEND_DEVICE_TYPE_IGPU) && std::strcmp(ggml_backend_dev_name(d), device) == 0) {
            dev = d;
        }
    }
    if (!dev) {
        put(err, err_len, std::string("no graphics device named ") + device);
        return nullptr;
    }
    {
        std::lock_guard<std::mutex> g(g_log.lock);
        g_log.offloaded = -1;
        g_log.layers = -1;
        g_log.last_error.clear();
    }
    ggml_backend_dev_t devices[2] = { dev, nullptr };
    llama_model_params mp = llama_model_default_params();
    mp.devices = devices;
    mp.n_gpu_layers = -1;  // every layer
    mp.split_mode = LLAMA_SPLIT_MODE_NONE;
    mp.main_gpu = 0;
    llama_model * m = llama_model_load_from_file(model, mp);
    int off, lay;
    std::string last;
    {
        std::lock_guard<std::mutex> g(g_log.lock);
        off = g_log.offloaded;
        lay = g_log.layers;
        last = g_log.last_error;
    }
    if (offloaded) {
        *offloaded = off;
    }
    if (layers) {
        *layers = lay;
    }
    if (!m) {
        put(err, err_len, "the model couldn't be loaded on the card" + (last.empty() ? std::string() : ": " + last));
        return nullptr;
    }
    llama_context_params cp = llama_context_default_params();
    cp.n_ctx = (uint32_t) n_ctx;
    cp.n_batch = (uint32_t) n_batch;
    cp.n_ubatch = (uint32_t) n_ubatch;
    cp.n_seq_max = 1;
    cp.n_threads = threads;
    cp.n_threads_batch = threads;
    cp.flash_attn_type = LLAMA_FLASH_ATTN_TYPE_AUTO;
    cp.offload_kqv = true;
    llama_context * c = llama_init_from_model(m, cp);
    if (!c) {
        {
            std::lock_guard<std::mutex> g(g_log.lock);
            last = g_log.last_error;
        }
        llama_model_free(m);
        put(err, err_len, "the model's context couldn't be made on the card" + (last.empty() ? std::string() : ": " + last));
        return nullptr;
    }
    mtmd_context_params mtp = mtmd_context_params_default();
    mtp.use_gpu = true;
    mtp.device = dev;
    mtp.print_timings = false;
    mtp.n_threads = threads;
    mtp.flash_attn_type = LLAMA_FLASH_ATTN_TYPE_AUTO;
    mtp.warmup = false;
    mtp.image_min_tokens = image_tokens;
    mtp.image_max_tokens = image_tokens;
    mtmd_context * x = mtmd_init_from_file(mmproj, m, mtp);
    if (!x) {
        {
            std::lock_guard<std::mutex> g(g_log.lock);
            last = g_log.last_error;
        }
        llama_free(c);
        llama_model_free(m);
        put(err, err_len, "the vision projector couldn't be loaded on the card" + (last.empty() ? std::string() : ": " + last));
        return nullptr;
    }
    auto * j = new pp_judge();
    j->model = m;
    j->ctx = c;
    j->mtmd = x;
    j->n_batch = n_batch;
    return j;
}

// The media marker the prompt carries where each image goes.
const char * pp_judge_marker(void) {
    return mtmd_default_marker();
}

// One question: `prompt` (with one marker per image, special tokens
// parsed, BOS added) and two images (encoded files: JPEG, PNG).  Fills
// the `top` most likely first tokens: their ids, log-probabilities over
// the whole vocabulary, and their text, each piece ending in '\n' ('\n'
// inside a piece is written as a space).  `*n_prompt` is the prompt's
// length in tokens.  Returns 0, or an error code with `err` filled.
int pp_judge_ask(pp_judge * j, const char * prompt, const unsigned char * a, size_t a_len, const unsigned char * b, size_t b_len, int top,
                 int * ids, float * logprobs, char * pieces, size_t pieces_len, int * n_prompt, char * err, size_t err_len) {
    if (!j || top <= 0) {
        put(err, err_len, "no judge");
        return 1;
    }
    mtmd_helper_init_opt opt = mtmd_helper_init_opt_default();
    auto wa = mtmd_helper_bitmap_init_from_buf(j->mtmd, a, a_len, false, opt);
    auto wb = mtmd_helper_bitmap_init_from_buf(j->mtmd, b, b_len, false, opt);
    if (!wa.bitmap || !wb.bitmap) {
        if (wa.bitmap) mtmd_bitmap_free(wa.bitmap);
        if (wb.bitmap) mtmd_bitmap_free(wb.bitmap);
        put(err, err_len, "an image couldn't be decoded");
        return 2;
    }
    const mtmd_bitmap * bitmaps[2] = { wa.bitmap, wb.bitmap };
    mtmd_input_text text = { prompt, std::strlen(prompt), true, true };
    mtmd_input_chunks * chunks = mtmd_input_chunks_init();
    int32_t rc = mtmd_tokenize(j->mtmd, chunks, &text, bitmaps, 2);
    mtmd_bitmap_free(wa.bitmap);
    mtmd_bitmap_free(wb.bitmap);
    if (rc != 0) {
        mtmd_input_chunks_free(chunks);
        put(err, err_len, "the prompt couldn't be tokenized (" + std::to_string(rc) + ")");
        return 3;
    }
    if (n_prompt) {
        *n_prompt = (int) mtmd_helper_get_n_tokens(chunks);
    }
    llama_memory_clear(llama_get_memory(j->ctx), true);
    llama_pos new_n_past = 0;
    rc = mtmd_helper_eval_chunks(j->mtmd, j->ctx, chunks, 0, 0, j->n_batch, true, &new_n_past);
    mtmd_input_chunks_free(chunks);
    if (rc != 0) {
        std::string last;
        {
            std::lock_guard<std::mutex> g(g_log.lock);
            last = g_log.last_error;
        }
        put(err, err_len, "the card couldn't evaluate the prompt (" + std::to_string(rc) + ")" + (last.empty() ? std::string() : ": " + last));
        return 4;
    }
    const float * logits = llama_get_logits_ith(j->ctx, -1);
    const llama_vocab * vocab = llama_model_get_vocab(j->model);
    const int n_vocab = llama_vocab_n_tokens(vocab);
    if (!logits || n_vocab <= 0) {
        put(err, err_len, "no logits");
        return 5;
    }
    // Softmax over the whole vocabulary, as llama-server's top_logprobs.
    float max_l = logits[0];
    for (int i = 1; i < n_vocab; i++) {
        max_l = std::max(max_l, logits[i]);
    }
    double sum = 0.0;
    for (int i = 0; i < n_vocab; i++) {
        sum += std::exp((double) logits[i] - max_l);
    }
    const double log_z = std::log(sum) + max_l;
    std::vector<int> order(n_vocab);
    for (int i = 0; i < n_vocab; i++) {
        order[i] = i;
    }
    const int k = std::min(top, n_vocab);
    std::partial_sort(order.begin(), order.begin() + k, order.end(), [&](int x, int y) { return logits[x] > logits[y]; });
    std::string text_out;
    char buf[256];
    for (int i = 0; i < k; i++) {
        const int id = order[i];
        ids[i] = id;
        logprobs[i] = (float) ((double) logits[id] - log_z);
        int n = llama_token_to_piece(vocab, id, buf, sizeof(buf) - 1, 0, false);
        std::string piece = n > 0 ? std::string(buf, (size_t) n) : std::string();
        std::replace(piece.begin(), piece.end(), '\n', ' ');
        text_out += piece;
        text_out += '\n';
    }
    put(pieces, pieces_len, text_out);
    return 0;
}

void pp_judge_free(pp_judge * j) {
    if (!j) {
        return;
    }
    if (j->mtmd) mtmd_free(j->mtmd);
    if (j->ctx) llama_free(j->ctx);
    if (j->model) llama_model_free(j->model);
    delete j;
}

// Lower-cased device description contains a software rasterizer's name.
int pp_judge_is_software(const char * description) {
    std::string d = lower(description ? description : "");
    return d.find("llvmpipe") != std::string::npos || d.find("swiftshader") != std::string::npos || d.find("lavapipe") != std::string::npos;
}

}  // extern "C"
