#include "engine/framework/tokenizers/sentencepiece.h"
#include "gguf.h"
#include "ggml.h"
#include <filesystem>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>
#include <string>
#include <vector>

static std::string bytes(const std::filesystem::path &path) {
    std::ifstream file(path, std::ios::binary);
    if (!file) throw std::runtime_error("Cannot read sidecar");
    return {std::istreambuf_iterator<char>(file), {}};
}

static void verify_embedded(const std::filesystem::path &weights,
                            const std::filesystem::path &tokenizer,
                            const std::filesystem::path &config) {
    ggml_context *tensors = nullptr;
    auto *gguf = gguf_init_from_file(weights.string().c_str(), gguf_init_params{true, &tensors});
    if (!gguf) throw std::runtime_error("Cannot read pinned GGUF metadata");
    try {
        auto names = gguf_find_key(gguf, "audiocpp.embedded_files.names");
        auto offsets = gguf_find_key(gguf, "audiocpp.embedded_files.offsets");
        auto data = gguf_find_key(gguf, "audiocpp.embedded_files.data");
        if (names < 0 || offsets < 0 || data < 0 ||
            gguf_get_kv_type(gguf, names) != GGUF_TYPE_ARRAY || gguf_get_arr_type(gguf, names) != GGUF_TYPE_STRING ||
            gguf_get_kv_type(gguf, offsets) != GGUF_TYPE_ARRAY || gguf_get_arr_type(gguf, offsets) != GGUF_TYPE_UINT64 ||
            gguf_get_kv_type(gguf, data) != GGUF_TYPE_ARRAY || gguf_get_arr_type(gguf, data) != GGUF_TYPE_UINT8 ||
            gguf_get_arr_n(gguf, offsets) != gguf_get_arr_n(gguf, names) + 1)
            throw std::runtime_error("Unexpected embedded GGUF layout");
        auto *positions = static_cast<const uint64_t *>(gguf_get_arr_data(gguf, offsets));
        auto *blob = static_cast<const char *>(gguf_get_arr_data(gguf, data));
        auto size = gguf_get_arr_n(gguf, data);
        int verified = 0;
        for (size_t i = 0; i < gguf_get_arr_n(gguf, names); ++i) {
            std::string name = gguf_get_arr_str(gguf, names, i);
            const auto *path = name == "t5gemma-b-b-ul2/tokenizer.model" ? &tokenizer :
                               name == "model_config.json" ? &config : nullptr;
            if (!path) continue;
            if (positions[i] > positions[i + 1] || positions[i + 1] > size)
                throw std::runtime_error("Embedded sidecar range invalid");
            if (bytes(*path) != std::string(blob + positions[i], positions[i + 1] - positions[i]))
                throw std::runtime_error("Extracted sidecar differs from pinned GGUF");
            ++verified;
        }
        if (verified != 2) throw std::runtime_error("Missing/duplicate embedded tokenizer or model config");
    } catch (...) {
        gguf_free(gguf);
        if (tensors) ggml_free(tensors);
        throw;
    }
    gguf_free(gguf);
    if (tensors) ggml_free(tensors);
}

static uint32_t read_u32(std::istream &stream) {
    unsigned char raw[4];
    if (!stream.read(reinterpret_cast<char *>(raw), 4)) throw std::runtime_error("Incomplete token request");
    return uint32_t(raw[0]) | uint32_t(raw[1]) << 8 | uint32_t(raw[2]) << 16 | uint32_t(raw[3]) << 24;
}

int wmain(int argc, wchar_t **argv) {
    try {
        if (argc != 6) throw std::runtime_error("Expected weights tokenizer config requests.bin output.json");
        verify_embedded(argv[1], argv[2], argv[3]);
        auto pieces = engine::tokenizers::load_sentencepiece_model(argv[2]);
        std::ifstream input(argv[4], std::ios::binary);
        auto count = read_u32(input);
        if (count == 0 || count > 4096) throw std::runtime_error("Request count outside helper bound");
        if (std::filesystem::exists(argv[5])) throw std::runtime_error("Refusing to replace oracle output");
        std::ofstream output(argv[5], std::ios::binary);
        if (!output) throw std::runtime_error("Cannot create oracle output");
        output << "{\"tokenizerEmbeddedMatches\":true,\"modelConfigEmbeddedMatches\":true,\"samples\":[";
        for (uint32_t n = 0; n < count; ++n) {
            auto size = read_u32(input);
            if (size == 0 || size > 16384) throw std::runtime_error("Prompt outside tokenizer helper byte bound");
            std::string text(size, '\0');
            if (!input.read(text.data(), size)) throw std::runtime_error("Incomplete UTF8 prompt");
            auto tokens = engine::tokenizers::tokenize_sentencepiece(pieces, text);
            if (n) output << ',';
            output << "{\"tokenIds\":[";
            for (size_t i = 0; i < tokens.size(); ++i) {
                if (i) output << ',';
                output << tokens[i];
            }
            output << "]}";
        }
        if (input.peek() != std::char_traits<char>::eof()) throw std::runtime_error("Trailing token request bytes");
        output << "]}\n";
        output.flush();
        if (!output) throw std::runtime_error("Oracle output write failed");
        return 0;
    } catch (const std::exception &error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
