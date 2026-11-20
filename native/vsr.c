// NVIDIA RTX Video Super Resolution through the Direct3D 11 video processor, the same path
// browsers and mpv use. Input: NV12 frames. Output: RGBA at the size shown on screen.
#define COBJMACROS
#include "vsr.h"

#include <d3d11.h>
#include <dxgi.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// NVIDIA's private video processor extension; method 2 = super resolution.
static const GUID NV_PPE_GUID = {0xd43ce1b3, 0x1f4b, 0x48ac, {0xba, 0xee, 0xc3, 0xc2, 0x53, 0x75, 0xe6, 0xf7}};
typedef struct { UINT version, method, enable; } NvPpeExt;

struct Vsr {
	ID3D11Device *dev;
	ID3D11DeviceContext *ctx;
	ID3D11VideoDevice *vdev;
	ID3D11VideoContext *vctx;
	ID3D11VideoProcessorEnumerator *en;
	ID3D11VideoProcessor *vp;
	ID3D11Texture2D *in, *out, *staging;
	ID3D11VideoProcessorInputView *iv;
	ID3D11VideoProcessorOutputView *ov;
	int iw, ih, ow, oh;
};

#define RELEASE(x) do { if(x) { IUnknown_Release((IUnknown *)(x)); (x) = NULL; } } while(0)

void vsr_free(Vsr *v)
{
	if(!v)
		return;
	RELEASE(v->ov);
	RELEASE(v->iv);
	RELEASE(v->staging);
	RELEASE(v->out);
	RELEASE(v->in);
	RELEASE(v->vp);
	RELEASE(v->en);
	RELEASE(v->vctx);
	RELEASE(v->vdev);
	RELEASE(v->ctx);
	RELEASE(v->dev);
	free(v);
}

static IDXGIAdapter1 *nvidia_adapter(void)
{
	IDXGIFactory1 *f = NULL;
	if(FAILED(CreateDXGIFactory1(&IID_IDXGIFactory1, (void **)&f)))
		return NULL;
	IDXGIAdapter1 *a = NULL, *found = NULL;
	for(UINT i = 0; !found && IDXGIFactory1_EnumAdapters1(f, i, &a) != DXGI_ERROR_NOT_FOUND; i++)
	{
		DXGI_ADAPTER_DESC1 d;
		if(SUCCEEDED(IDXGIAdapter1_GetDesc1(a, &d)) && d.VendorId == 0x10DE)
			found = a;
		else
			IDXGIAdapter1_Release(a);
	}
	IDXGIFactory1_Release(f);
	return found;
}

static Vsr *fail(Vsr *v, char *err, size_t n, const char *msg, HRESULT hr)
{
	snprintf(err, n, "%s (0x%08lx)", msg, (unsigned long)hr);
	vsr_free(v);
	return NULL;
}

Vsr *vsr_create(int iw, int ih, int ow, int oh, char *err, size_t n)
{
	Vsr *v = calloc(1, sizeof(Vsr));
	if(!v)
		return NULL;
	v->iw = iw, v->ih = ih, v->ow = ow, v->oh = oh;
	IDXGIAdapter1 *a = nvidia_adapter();
	if(!a)
		return fail(v, err, n, "RTX Video needs an NVIDIA graphics card", 0);
	D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0};
	HRESULT hr = D3D11CreateDevice((IDXGIAdapter *)a, D3D_DRIVER_TYPE_UNKNOWN, NULL, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
			levels, 2, D3D11_SDK_VERSION, &v->dev, NULL, &v->ctx);
	IDXGIAdapter1_Release(a);
	if(FAILED(hr))
		return fail(v, err, n, "Could not open the graphics card for RTX Video", hr);
	if(FAILED(hr = ID3D11Device_QueryInterface(v->dev, &IID_ID3D11VideoDevice, (void **)&v->vdev))
		|| FAILED(hr = ID3D11DeviceContext_QueryInterface(v->ctx, &IID_ID3D11VideoContext, (void **)&v->vctx)))
		return fail(v, err, n, "The graphics driver has no video processor", hr);

	D3D11_VIDEO_PROCESSOR_CONTENT_DESC cd = {0};
	cd.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
	cd.InputFrameRate.Numerator = 60;
	cd.InputFrameRate.Denominator = 1;
	cd.InputWidth = iw;
	cd.InputHeight = ih;
	cd.OutputFrameRate = cd.InputFrameRate;
	cd.OutputWidth = ow;
	cd.OutputHeight = oh;
	cd.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;
	if(FAILED(hr = ID3D11VideoDevice_CreateVideoProcessorEnumerator(v->vdev, &cd, &v->en))
		|| FAILED(hr = ID3D11VideoDevice_CreateVideoProcessor(v->vdev, v->en, 0, &v->vp)))
		return fail(v, err, n, "Could not create the video processor", hr);

	D3D11_TEXTURE2D_DESC td = {0};
	td.Width = iw;
	td.Height = ih;
	td.MipLevels = 1;
	td.ArraySize = 1;
	td.Format = DXGI_FORMAT_NV12;
	td.SampleDesc.Count = 1;
	td.Usage = D3D11_USAGE_DEFAULT;
	td.BindFlags = D3D11_BIND_RENDER_TARGET;
	if(FAILED(hr = ID3D11Device_CreateTexture2D(v->dev, &td, NULL, &v->in)))
		return fail(v, err, n, "Could not create the input surface", hr);
	td.Width = ow;
	td.Height = oh;
	td.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
	if(FAILED(hr = ID3D11Device_CreateTexture2D(v->dev, &td, NULL, &v->out)))
		return fail(v, err, n, "Could not create the output surface", hr);
	td.Usage = D3D11_USAGE_STAGING;
	td.BindFlags = 0;
	td.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
	if(FAILED(hr = ID3D11Device_CreateTexture2D(v->dev, &td, NULL, &v->staging)))
		return fail(v, err, n, "Could not create the readback surface", hr);

	D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC ivd = {0};
	ivd.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
	D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC ovd = {0};
	ovd.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
	if(FAILED(hr = ID3D11VideoDevice_CreateVideoProcessorInputView(v->vdev, (ID3D11Resource *)v->in, v->en, &ivd, &v->iv))
		|| FAILED(hr = ID3D11VideoDevice_CreateVideoProcessorOutputView(v->vdev, (ID3D11Resource *)v->out, v->en, &ovd, &v->ov)))
		return fail(v, err, n, "Could not set up the video processor views", hr);

	ID3D11VideoContext_VideoProcessorSetStreamFrameFormat(v->vctx, v->vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
	ID3D11VideoContext_VideoProcessorSetStreamAutoProcessingMode(v->vctx, v->vp, 0, FALSE);
	D3D11_VIDEO_PROCESSOR_COLOR_SPACE out_cs = {0}; // full-range RGB
	ID3D11VideoContext_VideoProcessorSetOutputColorSpace(v->vctx, v->vp, &out_cs);

	NvPpeExt ext = {1, 2, 1};
	if(FAILED(hr = ID3D11VideoContext_VideoProcessorSetStreamExtension(v->vctx, v->vp, 0, &NV_PPE_GUID, sizeof(ext), &ext)))
		return fail(v, err, n, "This NVIDIA driver does not offer RTX Video Super Resolution", hr);
	return v;
}

int vsr_size_matches(const Vsr *v, int iw, int ih, int ow, int oh)
{
	return v && v->iw == iw && v->ih == ih && v->ow == ow && v->oh == oh;
}

int vsr_process(Vsr *v, const uint8_t *nv12, int full_range, int bt601, uint8_t *rgba)
{
	D3D11_VIDEO_PROCESSOR_COLOR_SPACE cs = {0};
	cs.YCbCr_Matrix = bt601 ? 0 : 1;
	cs.Nominal_Range = full_range ? D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255 : D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235;
	ID3D11VideoContext_VideoProcessorSetStreamColorSpace(v->vctx, v->vp, 0, &cs);
	// For NV12 the chroma plane follows the luma rows directly, as in our buffer.
	ID3D11DeviceContext_UpdateSubresource(v->ctx, (ID3D11Resource *)v->in, 0, NULL, nv12, v->iw, 0);
	D3D11_VIDEO_PROCESSOR_STREAM st = {0};
	st.Enable = TRUE;
	st.pInputSurface = v->iv;
	if(FAILED(ID3D11VideoContext_VideoProcessorBlt(v->vctx, v->vp, v->ov, 0, 1, &st)))
		return -1;
	// shortcut: reads the upscaled frame back to the CPU (about 15 MB per 1440p frame); share the texture
	// with the DX12 renderer instead if the extra copy costs too much.
	ID3D11DeviceContext_CopyResource(v->ctx, (ID3D11Resource *)v->staging, (ID3D11Resource *)v->out);
	D3D11_MAPPED_SUBRESOURCE m;
	if(FAILED(ID3D11DeviceContext_Map(v->ctx, (ID3D11Resource *)v->staging, 0, D3D11_MAP_READ, 0, &m)))
		return -1;
	for(int y = 0; y < v->oh; y++)
		memcpy(rgba + (size_t)y * v->ow * 4, (const uint8_t *)m.pData + (size_t)y * m.RowPitch, (size_t)v->ow * 4);
	ID3D11DeviceContext_Unmap(v->ctx, (ID3D11Resource *)v->staging, 0);
	return 0;
}
