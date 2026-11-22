// Imports an area from a TomBox map server (tools/rage/mapview/server.py) into the project.
//
// Server protocol (see server.py):
//   GET /api/info                    JSON: tile size, tiles [[x, y, n_full, n_lod, n_slod], ...]
//   GET /api/tile?x=&y=&level=       u32 n, n * 12 f32 (pos xyz, quat xyzw, scale xy, scale z,
//                                    draw distance, 2 unused), n u32 model hash, n u8 resolved,
//                                    n u8 level
//   GET /api/model/<hash hex>        u32 header length, JSON {name, parts[{nv, nt, normals, uvs,
//                                    tex}], errors}, then per part f32 positions, [f32 normals],
//                                    [f32 uvs], u32 indices
//   GET /api/tex/<key hex>           PNG (largest mip <= 512 px)
//
// Coordinates: the game is right-handed, Z up, metres; Unreal is left-handed, Z up,
// centimetres. Positions map (x, y, z) -> (x, -y, z) * 100 (mirror in Y), so triangle winding
// is reversed and rotations are mirrored. A placement's stored quaternion is the inverse of
// its rotation (world = conj(q) v q + p, see tools/rage/placements.py); the rotation conj(q) =
// (-x, -y, -z, w), mirrored in Y, is (x, -y, z, w) in Unreal.

#include "TomBoxImporter.h"
#include "TomBoxImportOptions.h"

#include "HttpModule.h"
#include "HttpManager.h"
#include "Interfaces/IHttpRequest.h"
#include "Interfaces/IHttpResponse.h"
#include "Dom/JsonObject.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "MeshDescription.h"
#include "StaticMeshAttributes.h"
#include "Engine/StaticMesh.h"
#include "Engine/Texture2D.h"
#include "Materials/Material.h"
#include "Materials/MaterialInstanceConstant.h"
#include "Materials/MaterialExpressionTextureSampleParameter2D.h"
#include "Materials/MaterialExpressionVectorParameter.h"
#include "Materials/MaterialExpressionConstant.h"
#include "MaterialEditingLibrary.h"
#include "IImageWrapperModule.h"
#include "IImageWrapper.h"
#include "AssetRegistry/AssetRegistryModule.h"
#include "UObject/SavePackage.h"
#include "Misc/PackageName.h"
#include "Misc/ScopedSlowTask.h"
#include "Engine/World.h"
#include "Components/HierarchicalInstancedStaticMeshComponent.h"
#include "Engine/DirectionalLight.h"
#include "Components/DirectionalLightComponent.h"
#include "Engine/SkyLight.h"
#include "Components/SkyLightComponent.h"
#include "Components/SkyAtmosphereComponent.h"
#include "Engine/ExponentialHeightFog.h"
#include "Modules/ModuleManager.h"

DEFINE_LOG_CATEGORY_STATIC(LogTomBoxImport, Log, All);

#define LOCTEXT_NAMESPACE "TomBoxImporter"

namespace
{
	constexpr double MetresToCm = 100.0;

	bool SaveAsset(UObject* Asset)
	{
		UPackage* Package = Asset->GetOutermost();
		const bool bMap = Asset->IsA<UWorld>();
		const FString File = FPackageName::LongPackageNameToFilename(Package->GetName(),
			bMap ? FPackageName::GetMapPackageExtension() : FPackageName::GetAssetPackageExtension());
		FSavePackageArgs Args;
		Args.TopLevelFlags = RF_Public | RF_Standalone;
		Args.SaveFlags = SAVE_NoError;
		return UPackage::SavePackage(Package, Asset, *File, Args);
	}

	template <typename T>
	T* LoadExisting(const FString& PackageName, const FString& AssetName)
	{
		if (!FPackageName::DoesPackageExist(PackageName))
		{
			return nullptr;
		}
		return LoadObject<T>(nullptr, *(PackageName + TEXT(".") + AssetName), nullptr, LOAD_NoWarn | LOAD_Quiet);
	}

	FString HashName(uint32 Hash) { return FString::Printf(TEXT("%08x"), Hash); }

	/** Bytes [At, At + Count * sizeof(T)) of Data as T, or false when out of range. */
	template <typename T>
	bool ReadArray(const TArray<uint8>& Data, int64& At, int64 Count, TArray<T>& Out)
	{
		const int64 Bytes = Count * int64(sizeof(T));
		if (Count < 0 || At + Bytes > Data.Num())
		{
			return false;
		}
		Out.SetNumUninitialized(Count);
		FMemory::Memcpy(Out.GetData(), Data.GetData() + At, Bytes);
		At += Bytes;
		return true;
	}

	FVector3f GameToUe(const float* P) { return FVector3f(P[0], -P[1], P[2]); }
}

FString FTomBoxImportReport::ToString() const
{
	FString S = FString::Printf(
		TEXT("placements %d (no model in the package: %d); models %d: built %d, reused %d, failed %d; ")
		TEXT("parts skipped %d; textures built %d, failed %d; map %s"),
		Placements, PlacementsNoModel, Models, ModelsBuilt, ModelsReused, ModelsFailed, PartsSkipped,
		TexturesBuilt, TexturesFailed, *MapPackage);
	for (const FString& E : Errors)
	{
		S += TEXT("\n  ") + E;
	}
	return S;
}

FTomBoxImporter::FTomBoxImporter(const UTomBoxImportOptions& InOptions) : Options(InOptions) {}

bool FTomBoxImporter::Get(const FString& Path, TArray<uint8>& Out, FString& Error) const
{
	TSharedRef<IHttpRequest, ESPMode::ThreadSafe> Request = FHttpModule::Get().CreateRequest();
	Request->SetURL(Options.ServerUrl + Path);
	Request->SetVerb(TEXT("GET"));
	Request->SetTimeout(120.f);
	if (!Request->ProcessRequest())
	{
		Error = TEXT("request not started");
		return false;
	}
	while (Request->GetStatus() == EHttpRequestStatus::NotStarted || Request->GetStatus() == EHttpRequestStatus::Processing)
	{
		FHttpModule::Get().GetHttpManager().Tick(0.01f);
		FPlatformProcess::Sleep(0.002f);
	}
	FHttpResponsePtr Response = Request->GetResponse();
	if (Request->GetStatus() != EHttpRequestStatus::Succeeded || !Response.IsValid())
	{
		Error = FString::Printf(TEXT("%s: no response (is the map server running at %s?)"), *Path, *Options.ServerUrl);
		return false;
	}
	if (Response->GetResponseCode() != 200)
	{
		Error = FString::Printf(TEXT("%s: HTTP %d %s"), *Path, Response->GetResponseCode(), *Response->GetContentAsString().Left(200));
		return false;
	}
	Out = Response->GetContent();
	return true;
}

bool FTomBoxImporter::FetchArea(TArray<FPlacement>& Out, FTomBoxImportReport& Report)
{
	TArray<uint8> Data;
	FString Error;
	if (!Get(TEXT("/api/info"), Data, Error))
	{
		Report.Errors.Add(Error);
		return false;
	}
	FUTF8ToTCHAR Conv(reinterpret_cast<const ANSICHAR*>(Data.GetData()), Data.Num());
	TSharedPtr<FJsonObject> Info;
	if (!FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(FString(Conv.Length(), Conv.Get())), Info) || !Info.IsValid())
	{
		Report.Errors.Add(TEXT("/api/info: not JSON"));
		return false;
	}
	TileSize = Info->GetNumberField(TEXT("tile"));
	const int32 Level = FMath::Clamp(Options.DetailLevel, 0, 2);
	const FVector2D C = Options.CenterMeters;
	const double R = Options.RadiusMeters;
	for (const TSharedPtr<FJsonValue>& V : Info->GetArrayField(TEXT("tiles")))
	{
		const TArray<TSharedPtr<FJsonValue>>& T = V->AsArray();
		if (T.Num() < 5)
		{
			continue;
		}
		const int32 TX = int32(T[0]->AsNumber()), TY = int32(T[1]->AsNumber());
		if (T[2 + Level]->AsNumber() <= 0)
		{
			continue;
		}
		// Closest point of the tile to the centre.
		const double NX = FMath::Clamp(C.X, TX * TileSize, (TX + 1) * TileSize);
		const double NY = FMath::Clamp(C.Y, TY * TileSize, (TY + 1) * TileSize);
		if (FMath::Square(NX - C.X) + FMath::Square(NY - C.Y) > R * R)
		{
			continue;
		}
		if (!Get(FString::Printf(TEXT("/api/tile?x=%d&y=%d&level=%d"), TX, TY, Level), Data, Error))
		{
			Report.Errors.Add(Error);
			return false;
		}
		int64 At = 0;
		TArray<uint32> Count;
		TArray<float> F;
		TArray<uint32> Names;
		TArray<uint8> Resolved;
		if (!ReadArray(Data, At, 1, Count) || !ReadArray(Data, At, int64(Count[0]) * 12, F) || !ReadArray(Data, At, Count[0], Names)
			|| !ReadArray(Data, At, Count[0], Resolved))
		{
			Report.Errors.Add(FString::Printf(TEXT("tile %d,%d: truncated"), TX, TY));
			return false;
		}
		for (uint32 I = 0; I < Count[0]; ++I)
		{
			const float* P = &F[I * 12];
			if (FMath::Square(P[0] - C.X) + FMath::Square(P[1] - C.Y) > R * R)
			{
				continue;
			}
			++Report.Placements;
			if (!Resolved[I])
			{
				++Report.PlacementsNoModel;
				continue;
			}
			FPlacement Pl;
			Pl.Position = FVector(P[0], P[1], P[2]);
			Pl.Stored = FQuat4d(P[3], P[4], P[5], P[6]);
			Pl.ScaleXY = P[7];
			Pl.ScaleZ = P[8];
			Pl.Model = Names[I];
			Out.Add(Pl);
		}
	}
	return true;
}

UMaterial* FTomBoxImporter::BaseMaterial(bool bTextured)
{
	const FString Name = bTextured ? TEXT("M_TomBox_Textured") : TEXT("M_TomBox_Plain");
	const FString PackageName = Options.ContentPath / TEXT("Materials") / Name;
	if (UMaterial* Existing = LoadExisting<UMaterial>(PackageName, Name))
	{
		return Existing;
	}
	UPackage* Package = CreatePackage(*PackageName);
	UMaterial* M = NewObject<UMaterial>(Package, *Name, RF_Public | RF_Standalone);
	M->TwoSided = true;
	if (bTextured)
	{
		auto* Tex = Cast<UMaterialExpressionTextureSampleParameter2D>(
			UMaterialEditingLibrary::CreateMaterialExpression(M, UMaterialExpressionTextureSampleParameter2D::StaticClass(), -400, 0));
		Tex->ParameterName = TEXT("BaseColorTex");
		Tex->Texture = LoadObject<UTexture2D>(nullptr, TEXT("/Engine/EngineResources/DefaultTexture.DefaultTexture"));
		Tex->SamplerType = SAMPLERTYPE_Color;
		UMaterialEditingLibrary::ConnectMaterialProperty(Tex, TEXT("RGB"), MP_BaseColor);
	}
	else
	{
		auto* Col = Cast<UMaterialExpressionVectorParameter>(
			UMaterialEditingLibrary::CreateMaterialExpression(M, UMaterialExpressionVectorParameter::StaticClass(), -400, 0));
		Col->ParameterName = TEXT("Color");
		Col->DefaultValue = FLinearColor(0.5f, 0.5f, 0.5f);
		UMaterialEditingLibrary::ConnectMaterialProperty(Col, TEXT(""), MP_BaseColor);
	}
	auto* Rough = Cast<UMaterialExpressionConstant>(
		UMaterialEditingLibrary::CreateMaterialExpression(M, UMaterialExpressionConstant::StaticClass(), -400, 200));
	Rough->R = 0.8f;
	UMaterialEditingLibrary::ConnectMaterialProperty(Rough, TEXT(""), MP_Roughness);
	UMaterialEditingLibrary::RecompileMaterial(M);
	FAssetRegistryModule::AssetCreated(M);
	SaveAsset(M);
	return M;
}

UTexture2D* FTomBoxImporter::BuildTexture(const FString& Key, FTomBoxImportReport& Report)
{
	const FString Name = TEXT("T_") + Key;
	const FString PackageName = Options.ContentPath / TEXT("Textures") / Name;
	if (UTexture2D* Existing = LoadExisting<UTexture2D>(PackageName, Name))
	{
		return Existing;
	}
	TArray<uint8> Png;
	FString Error;
	if (!Get(TEXT("/api/tex/") + Key, Png, Error))
	{
		++Report.TexturesFailed;
		if (Report.Errors.Num() < 20)
		{
			Report.Errors.Add(Error);
		}
		return nullptr;
	}
	IImageWrapperModule& Wrappers = FModuleManager::LoadModuleChecked<IImageWrapperModule>(TEXT("ImageWrapper"));
	TSharedPtr<IImageWrapper> Image = Wrappers.CreateImageWrapper(EImageFormat::PNG);
	TArray64<uint8> Raw;
	if (!Image.IsValid() || !Image->SetCompressed(Png.GetData(), Png.Num()) || !Image->GetRaw(ERGBFormat::BGRA, 8, Raw))
	{
		++Report.TexturesFailed;
		return nullptr;
	}
	UPackage* Package = CreatePackage(*PackageName);
	UTexture2D* T = NewObject<UTexture2D>(Package, *Name, RF_Public | RF_Standalone);
	T->Source.Init(Image->GetWidth(), Image->GetHeight(), 1, 1, TSF_BGRA8, Raw.GetData());
	T->SRGB = true;  // colour maps (DiffuseTex is sRGB-format in every sampled shader)
	T->CompressionSettings = TC_Default;
	T->PostEditChange();
	FAssetRegistryModule::AssetCreated(T);
	SaveAsset(T);
	++Report.TexturesBuilt;
	return T;
}

UMaterialInterface* FTomBoxImporter::MaterialForTexture(const FString& Key, FTomBoxImportReport& Report)
{
	if (UMaterialInterface** Cached = MaterialCache.Find(Key))
	{
		return *Cached;
	}
	const FString Name = TEXT("MI_") + Key;
	const FString PackageName = Options.ContentPath / TEXT("Materials") / Name;
	UMaterialInterface* Result = LoadExisting<UMaterialInstanceConstant>(PackageName, Name);
	if (!Result)
	{
		UTexture2D* Tex = BuildTexture(Key, Report);
		if (!Tex)
		{
			Result = Untextured;  // texture not decodable: grey
		}
		else
		{
			UPackage* Package = CreatePackage(*PackageName);
			UMaterialInstanceConstant* MI = NewObject<UMaterialInstanceConstant>(Package, *Name, RF_Public | RF_Standalone);
			MI->SetParentEditorOnly(BaseMaterial(true));
			MI->SetTextureParameterValueEditorOnly(FMaterialParameterInfo(TEXT("BaseColorTex")), Tex);
			MI->PostEditChange();
			FAssetRegistryModule::AssetCreated(MI);
			SaveAsset(MI);
			Result = MI;
		}
	}
	MaterialCache.Add(Key, Result);
	return Result;
}

UStaticMesh* FTomBoxImporter::BuildModel(uint32 Hash, FString& DisplayName, FTomBoxImportReport& Report)
{
	const FString Name = TEXT("SM_") + HashName(Hash);
	const FString PackageName = Options.ContentPath / TEXT("Models") / Name;
	if (UStaticMesh* Existing = LoadExisting<UStaticMesh>(PackageName, Name))
	{
		++Report.ModelsReused;
		DisplayName = Name;
		return Existing;
	}
	TArray<uint8> Data;
	FString Error;
	if (!Get(TEXT("/api/model/") + HashName(Hash), Data, Error))
	{
		++Report.ModelsFailed;
		if (Report.Errors.Num() < 20)
		{
			Report.Errors.Add(Error);
		}
		return nullptr;
	}
	int64 At = 0;
	TArray<uint32> HeaderLen;
	if (!ReadArray(Data, At, 1, HeaderLen) || At + HeaderLen[0] > Data.Num())
	{
		++Report.ModelsFailed;
		return nullptr;
	}
	FUTF8ToTCHAR Conv(reinterpret_cast<const ANSICHAR*>(Data.GetData() + At), HeaderLen[0]);
	At += HeaderLen[0];
	TSharedPtr<FJsonObject> Head;
	if (!FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(FString(Conv.Length(), Conv.Get())), Head) || !Head.IsValid())
	{
		++Report.ModelsFailed;
		return nullptr;
	}
	DisplayName = Head->GetStringField(TEXT("name"));
	Report.PartsSkipped += Head->GetArrayField(TEXT("errors")).Num();
	const TArray<TSharedPtr<FJsonValue>>& Parts = Head->GetArrayField(TEXT("parts"));
	if (Parts.Num() == 0)
	{
		++Report.ModelsFailed;
		return nullptr;
	}

	FMeshDescription Mesh;
	FStaticMeshAttributes Attributes(Mesh);
	Attributes.Register();
	TVertexAttributesRef<FVector3f> Positions = Attributes.GetVertexPositions();
	TVertexInstanceAttributesRef<FVector3f> Normals = Attributes.GetVertexInstanceNormals();
	TVertexInstanceAttributesRef<FVector2f> UVs = Attributes.GetVertexInstanceUVs();
	TPolygonGroupAttributesRef<FName> SlotNames = Attributes.GetPolygonGroupMaterialSlotNames();
	TArray<UMaterialInterface*> Materials;

	for (int32 PartIndex = 0; PartIndex < Parts.Num(); ++PartIndex)
	{
		const TSharedPtr<FJsonObject> Part = Parts[PartIndex]->AsObject();
		const int64 NV = int64(Part->GetNumberField(TEXT("nv")));
		const int64 NT = int64(Part->GetNumberField(TEXT("nt")));
		const bool bNormals = Part->GetBoolField(TEXT("normals"));
		const bool bUVs = Part->GetBoolField(TEXT("uvs"));
		FString Tex;
		const bool bTex = Part->TryGetStringField(TEXT("tex"), Tex);
		TArray<float> P, N, UV;
		TArray<uint32> Idx;
		if (!ReadArray(Data, At, NV * 3, P) || (bNormals && !ReadArray(Data, At, NV * 3, N)) || (bUVs && !ReadArray(Data, At, NV * 2, UV))
			|| !ReadArray(Data, At, NT * 3, Idx))
		{
			++Report.ModelsFailed;
			Report.Errors.Add(DisplayName + TEXT(": model data truncated"));
			return nullptr;
		}
		// Normals the model does not carry: area-weighted face normals (in game space).
		if (!bNormals)
		{
			N.SetNumZeroed(NV * 3);
			for (int64 T = 0; T < NT; ++T)
			{
				const uint32 A = Idx[T * 3], B = Idx[T * 3 + 1], C = Idx[T * 3 + 2];
				if (A >= NV || B >= NV || C >= NV) continue;
				const FVector3f PA(P[A * 3], P[A * 3 + 1], P[A * 3 + 2]), PB(P[B * 3], P[B * 3 + 1], P[B * 3 + 2]), PC(P[C * 3], P[C * 3 + 1], P[C * 3 + 2]);
				const FVector3f FN = FVector3f::CrossProduct(PB - PA, PC - PA);
				for (uint32 V : {A, B, C}) { N[V * 3] += FN.X; N[V * 3 + 1] += FN.Y; N[V * 3 + 2] += FN.Z; }
			}
		}
		const FPolygonGroupID Group = Mesh.CreatePolygonGroup();
		const FName Slot(*FString::Printf(TEXT("Part%d"), PartIndex));
		SlotNames[Group] = Slot;
		Materials.Add(bTex && Options.bImportTextures ? MaterialForTexture(Tex, Report) : Untextured);

		TArray<FVertexInstanceID> Instances;
		Instances.SetNumUninitialized(NV);
		for (int64 V = 0; V < NV; ++V)
		{
			const FVertexID Vertex = Mesh.CreateVertex();
			Positions[Vertex] = GameToUe(&P[V * 3]) * float(MetresToCm);
			const FVertexInstanceID Instance = Mesh.CreateVertexInstance(Vertex);
			Normals[Instance] = GameToUe(&N[V * 3]).GetSafeNormal();
			UVs.Set(Instance, 0, bUVs ? FVector2f(UV[V * 2], UV[V * 2 + 1]) : FVector2f::ZeroVector);
			Instances[V] = Instance;
		}
		for (int64 T = 0; T < NT; ++T)
		{
			const uint32 A = Idx[T * 3], B = Idx[T * 3 + 1], C = Idx[T * 3 + 2];
			if (A >= NV || B >= NV || C >= NV || A == B || B == C || A == C) continue;
			// Mirroring Y reverses the winding.
			Mesh.CreateTriangle(Group, TArray<FVertexInstanceID>{Instances[A], Instances[C], Instances[B]});
		}
	}

	UPackage* Package = CreatePackage(*PackageName);
	UStaticMesh* SM = NewObject<UStaticMesh>(Package, *Name, RF_Public | RF_Standalone);
	for (int32 I = 0; I < Materials.Num(); ++I)
	{
		const FName Slot(*FString::Printf(TEXT("Part%d"), I));
		SM->GetStaticMaterials().Add(FStaticMaterial(Materials[I], Slot, Slot));
	}
	FStaticMeshSourceModel& Source = SM->AddSourceModel();
	Source.BuildSettings.bRecomputeNormals = false;
	Source.BuildSettings.bRecomputeTangents = true;
	Source.BuildSettings.bGenerateLightmapUVs = false;
	Source.BuildSettings.bRemoveDegenerates = true;
	SM->NaniteSettings.bEnabled = Options.bNanite;
	SM->CreateMeshDescription(0, MoveTemp(Mesh));
	SM->CommitMeshDescription(0);
	SM->Build(true);
	SM->PostEditChange();
	FAssetRegistryModule::AssetCreated(SM);
	SaveAsset(SM);
	++Report.ModelsBuilt;
	return SM;
}

bool FTomBoxImporter::BuildMap(const TArray<FPlacement>& Placements, const TMap<uint32, UStaticMesh*>& Meshes,
	const TMap<uint32, FString>& Names, FTomBoxImportReport& Report)
{
	FString MapName = Options.MapName;
	FString PackageName = Options.ContentPath / TEXT("Maps") / MapName;
	for (int32 N = 2; FPackageName::DoesPackageExist(PackageName) || FindPackage(nullptr, *PackageName); ++N)
	{
		MapName = FString::Printf(TEXT("%s_%d"), *Options.MapName, N);
		PackageName = Options.ContentPath / TEXT("Maps") / MapName;
	}
	UPackage* Package = CreatePackage(*PackageName);
	UWorld* World = UWorld::CreateWorld(EWorldType::Editor, false, FName(*MapName), Package, false);
	World->SetFlags(RF_Public | RF_Standalone);

	// Placements are re-centred on the area centre (ground level kept).
	const FVector Origin(Options.CenterMeters.X, Options.CenterMeters.Y, 0.0);
	TMap<uint32, TArray<FTransform>> ByModel;
	for (const FPlacement& Pl : Placements)
	{
		if (!Meshes.Contains(Pl.Model))
		{
			continue;
		}
		const FVector Local = Pl.Position - Origin;
		const FVector Location(Local.X * MetresToCm, -Local.Y * MetresToCm, Local.Z * MetresToCm);
		FQuat4d Rotation(Pl.Stored.X, -Pl.Stored.Y, Pl.Stored.Z, Pl.Stored.W);
		Rotation.Normalize();
		ByModel.FindOrAdd(Pl.Model).Add(FTransform(Rotation, Location, FVector(Pl.ScaleXY, Pl.ScaleXY, Pl.ScaleZ)));
	}
	for (const TPair<uint32, TArray<FTransform>>& It : ByModel)
	{
		FActorSpawnParameters Params;
		Params.Name = MakeUniqueObjectName(World->PersistentLevel, AActor::StaticClass(), FName(*(TEXT("TB_") + HashName(It.Key))));
		AActor* Actor = World->SpawnActor<AActor>(AActor::StaticClass(), FTransform::Identity, Params);
		auto* Instances = NewObject<UHierarchicalInstancedStaticMeshComponent>(Actor, TEXT("Instances"));
		Instances->SetMobility(EComponentMobility::Static);
		Instances->SetCollisionEnabled(ECollisionEnabled::NoCollision);
		Actor->SetRootComponent(Instances);
		Actor->AddInstanceComponent(Instances);
		Instances->SetStaticMesh(Meshes[It.Key]);
		Instances->RegisterComponent();
		Instances->AddInstances(It.Value, false, false);
		const FString* Label = Names.Find(It.Key);
		Actor->SetActorLabel(Label ? *Label : HashName(It.Key));
	}
	// Light, sky and fog so the map renders when opened.
	ADirectionalLight* Sun = World->SpawnActor<ADirectionalLight>(FVector(0, 0, 50000), FRotator(-40, 35, 0));
	Sun->GetComponent()->SetAtmosphereSunLight(true);
	ASkyLight* Sky = World->SpawnActor<ASkyLight>(FVector(0, 0, 50000), FRotator::ZeroRotator);
	Sky->GetLightComponent()->SetMobility(EComponentMobility::Movable);
	Sky->GetLightComponent()->bRealTimeCapture = true;
	World->SpawnActor<ASkyAtmosphere>(FVector::ZeroVector, FRotator::ZeroRotator);
	World->SpawnActor<AExponentialHeightFog>(FVector::ZeroVector, FRotator::ZeroRotator);

	FAssetRegistryModule::AssetCreated(World);
	const bool bSaved = SaveAsset(World);
	Report.MapPackage = PackageName;
	World->DestroyWorld(false);
	if (!bSaved)
	{
		Report.Errors.Add(TEXT("map could not be saved: ") + PackageName);
	}
	return bSaved;
}

bool FTomBoxImporter::Run(FTomBoxImportReport& Report)
{
	TArray<FPlacement> Placements;
	if (!FetchArea(Placements, Report))
	{
		return false;
	}
	if (Placements.Num() == 0)
	{
		Report.Errors.Add(TEXT("no placements with a model in this area at this detail level"));
		return false;
	}
	TArray<uint32> Models;
	for (const FPlacement& Pl : Placements)
	{
		Models.AddUnique(Pl.Model);
	}
	Report.Models = Models.Num();

	{
		const FString Name = TEXT("MI_TomBox_Untextured");
		const FString PackageName = Options.ContentPath / TEXT("Materials") / Name;
		Untextured = LoadExisting<UMaterialInstanceConstant>(PackageName, Name);
		if (!Untextured)
		{
			UPackage* Package = CreatePackage(*PackageName);
			UMaterialInstanceConstant* MI = NewObject<UMaterialInstanceConstant>(Package, *Name, RF_Public | RF_Standalone);
			MI->SetParentEditorOnly(BaseMaterial(false));
			MI->PostEditChange();
			FAssetRegistryModule::AssetCreated(MI);
			SaveAsset(MI);
			Untextured = MI;
		}
	}

	FScopedSlowTask Task(float(Models.Num() + 1), LOCTEXT("Importing", "Importing map area"));
	if (!IsRunningCommandlet())
	{
		Task.MakeDialog(true);
	}
	TMap<uint32, UStaticMesh*> Meshes;
	TMap<uint32, FString> Names;
	for (int32 I = 0; I < Models.Num(); ++I)
	{
		if (Task.ShouldCancel())
		{
			Report.Errors.Add(TEXT("cancelled; the map holds the models built so far"));
			break;
		}
		Task.EnterProgressFrame(1.f, FText::Format(LOCTEXT("Model", "Model {0} / {1}"), I + 1, Models.Num()));
		FString Name;
		if (UStaticMesh* SM = BuildModel(Models[I], Name, Report))
		{
			Meshes.Add(Models[I], SM);
			Names.Add(Models[I], Name);
		}
		if (IsRunningCommandlet() && (I + 1) % 100 == 0)
		{
			UE_LOG(LogTomBoxImport, Display, TEXT("%d / %d models"), I + 1, Models.Num());
		}
	}
	Task.EnterProgressFrame(1.f, LOCTEXT("Map", "Placing models"));
	return BuildMap(Placements, Meshes, Names, Report);
}

#undef LOCTEXT_NAMESPACE
