// Flat C interface over chiaki-ng for nagito. Plain types only, so Rust never sees chiaki structs.
#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef NAGITO_BUILD
#define NAGITO_API __declspec(dllexport)
#else
#define NAGITO_API __declspec(dllimport)
#endif

enum { NAGITO_HOST_UNKNOWN = 0, NAGITO_HOST_READY = 1, NAGITO_HOST_STANDBY = 2 };

typedef struct NagitoHost {
	char addr[64];
	char name[64];
	char id[32];
	char app[64];
	int state;
	int ps5;
} NagitoHost;

typedef struct NagitoRegistered {
	char nickname[32];
	uint8_t mac[6];
	char regist_key[16];
	uint8_t rp_key[16];
} NagitoRegistered;

typedef struct NagitoStreamConfig {
	const char *host;
	int ps5;
	char regist_key[16];
	uint8_t rp_key[16];
	uint8_t account_id[8];
	int width, height, fps, bitrate_kbps;
	int hevc;
	int rumble, triggers; // DualSense feedback the user allows
	int motion, touchpad, haptics, mic;
	uint32_t button_map[16]; // PS button for each pad button in NAGITO_PAD_* order; 0 keeps the default
} NagitoStreamConfig;

// Pad buttons in the order button_map uses.
enum { NAGITO_PAD_A, NAGITO_PAD_B, NAGITO_PAD_X, NAGITO_PAD_Y, NAGITO_PAD_LEFT, NAGITO_PAD_RIGHT, NAGITO_PAD_UP, NAGITO_PAD_DOWN,
	NAGITO_PAD_L1, NAGITO_PAD_R1, NAGITO_PAD_L3, NAGITO_PAD_R3, NAGITO_PAD_START, NAGITO_PAD_BACK, NAGITO_PAD_TOUCHPAD, NAGITO_PAD_GUIDE };

// PIN: value 1 when the previous passcode was wrong. MIC: value 1 when muted.
enum { NAGITO_EV_CONNECTED = 0, NAGITO_EV_QUIT = 1, NAGITO_EV_PADS = 2, NAGITO_EV_PIN = 3, NAGITO_EV_MIC = 4, NAGITO_EV_VSR = 5 };
// VSR: value 1 when RTX Video is upscaling, 0 with the reason in text when it is not available.

// Frame flags for the video callback.
// RGBA: the frame is width*height*4 RGBA, already upscaled by RTX Video.
enum { NAGITO_FRAME_FULL_RANGE = 1, NAGITO_FRAME_BT601 = 2, NAGITO_FRAME_RGBA = 4 };

// Keyboard/mouse input, merged with the controllers.
typedef struct NagitoInput {
	uint32_t buttons; // ChiakiControllerButton mask
	uint8_t l2, r2;
	int16_t lx, ly, rx, ry;
} NagitoInput;

typedef struct NagitoStats {
	float rtt_ms, loss, bitrate_mbps, decode_ms;
} NagitoStats;

typedef struct NagitoCallbacks {
	void *user;
	// NV12: width*height luma bytes, then width*height/2 interleaved chroma bytes.
	void (*video)(void *user, const uint8_t *nv12, int width, int height, int flags);
	void (*audio)(void *user, const int16_t *pcm, size_t frames, int channels, int rate);
	void (*event)(void *user, int kind, int value, const char *text);
} NagitoCallbacks;

typedef void (*NagitoHostsCb)(void *user, const NagitoHost *hosts, int count);
typedef void (*NagitoRegistCb)(void *user, int ok, const NagitoRegistered *host);

NAGITO_API int nagito_init(void);

// extra: addresses outside this network (shared consoles) to ping as well.
NAGITO_API void *nagito_discovery_start(NagitoHostsCb cb, void *user, const char *const *extra, int extra_count);
NAGITO_API void nagito_discovery_stop(void *discovery);
NAGITO_API int nagito_wakeup(const char *host, const char regist_key[16], int ps5);

NAGITO_API void *nagito_regist_start(const char *host, int ps5, const uint8_t account_id[8], uint32_t pin, NagitoRegistCb cb, void *user);
NAGITO_API void nagito_regist_free(void *regist);

// Asks the router (UPnP) to forward the PS5 Remote Play ports to console_ip. Returns how many of the
// 5 mappings succeeded, or -1 when no UPnP router answered. external_ip gets the public address if known.
NAGITO_API int nagito_upnp_forward(const char *console_ip, char *external_ip, size_t external_ip_size);

NAGITO_API void *nagito_stream_start(const NagitoStreamConfig *cfg, const NagitoCallbacks *cb, char *err, size_t err_size);
NAGITO_API void nagito_stream_stop(void *stream);
NAGITO_API void nagito_stream_set_pin(void *stream, const char *pin);
NAGITO_API void nagito_stream_set_input(void *stream, const NagitoInput *input);
NAGITO_API void nagito_stream_stats(void *stream, NagitoStats *out);
// Upscale with NVIDIA RTX Video Super Resolution to out_w x out_h (the size on screen); on = 0 turns it off.
NAGITO_API void nagito_stream_set_vsr(void *stream, int on, int out_w, int out_h);
// Self-test: upscales a 1080p frame (white top half, black bottom) to 1440p. Returns 0 and the RGBA of a
// top and a bottom pixel, or -1 with the reason in err.
NAGITO_API int nagito_vsr_selftest(uint8_t top[4], uint8_t bottom[4], char *err, size_t err_size);
