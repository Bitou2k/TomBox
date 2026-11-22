#pragma once

#include "CoreMinimal.h"

class UTomBoxImportOptions;
class UStaticMesh;
class UTexture2D;
class UMaterialInterface;
class UMaterial;

/** Counts reported after an import. */
struct FTomBoxImportReport
{
	int32 Placements = 0;          // placements inside the area at the chosen detail level
	int32 PlacementsNoModel = 0;   // their model is not in the package (not imported)
	int32 Models = 0;              // distinct models placed
	int32 ModelsBuilt = 0;         // static meshes created this run
	int32 ModelsReused = 0;        // already present from an earlier import
	int32 ModelsFailed = 0;        // server could not decode the model (or no part decoded)
	int32 PartsSkipped = 0;        // parts the server reported as undecodable
	int32 TexturesBuilt = 0;
	int32 TexturesFailed = 0;      // undecodable texture (codec, short record, ...)
	FString MapPackage;
	TArray<FString> Errors;        // first few failure messages, for the log
	FString ToString() const;
};

/**
 * Imports an area from a TomBox map server into the open project. Synchronous; reports
 * progress through FScopedSlowTask. Returns false only when nothing could be imported
 * (server unreachable, empty area); partial failures are counted in the report.
 */
class TOMBOXLEVELIMPORT_API FTomBoxImporter
{
public:
	explicit FTomBoxImporter(const UTomBoxImportOptions& InOptions);
	bool Run(FTomBoxImportReport& Report);

private:
	struct FPlacement
	{
		FVector Position;  // game metres
		FQuat4d Stored;    // as stored (inverse rotation)
		double ScaleXY = 1, ScaleZ = 1;
		uint32 Model = 0;
	};

	bool Get(const FString& Path, TArray<uint8>& Out, FString& Error) const;
	bool FetchArea(TArray<FPlacement>& Out, FTomBoxImportReport& Report);
	UStaticMesh* BuildModel(uint32 Hash, FString& DisplayName, FTomBoxImportReport& Report);
	UMaterialInterface* MaterialForTexture(const FString& Key, FTomBoxImportReport& Report);
	UTexture2D* BuildTexture(const FString& Key, FTomBoxImportReport& Report);
	UMaterial* BaseMaterial(bool bTextured);
	bool BuildMap(const TArray<FPlacement>& Placements, const TMap<uint32, UStaticMesh*>& Meshes,
		const TMap<uint32, FString>& Names, FTomBoxImportReport& Report);

	const UTomBoxImportOptions& Options;
	double TileSize = 250.0;
	TMap<FString, UMaterialInterface*> MaterialCache;
	UMaterialInterface* Untextured = nullptr;
};
